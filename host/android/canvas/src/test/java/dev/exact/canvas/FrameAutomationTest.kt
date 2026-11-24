package dev.exact.canvas

import android.net.Uri
import android.os.Looper
import android.webkit.WebView
import androidx.webkit.JavaScriptExecutionException
import androidx.webkit.JavaScriptReplyProxy
import androidx.webkit.Navigation
import androidx.webkit.WebMessageCompat
import androidx.webkit.WebViewOutcomeReceiver
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import java.io.File
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Method
import java.util.concurrent.Callable

/**
 * `@exact/browser-automation` on a kept partitioned frame (LLP 1020.002 D9, §11 "Android as built"): scripts in the
 * page's world without awaiting, in a sandbox's world through its own channel, and refusals where Exact would need
 * something of its own in the page's world or the WebView has no worlds. The emulator drive measures the platform.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [31], manifest = Config.NONE, shadows = [
    NativesPartitionTest.Host::class, NativesPartitionTest.Compat::class, NativesPartitionTest.Features::class,
])
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class FrameAutomationTest {
    private val compat = NativesPartitionTest.Compat

    @Before fun clear() {
        NativesPartitionTest.Host.sent.clear(); compat.profiles.clear(); compat.worldListeners.clear(); compat.worldScripts.clear()
        NativesPartitionTest.Features.profiles = true; NativesPartitionTest.Features.navigation = true
        NativesPartitionTest.Features.worlds = true
        FrameAutomations.readers.clear()
    }

    private fun natives() = Natives(RuntimeEnvironment.getApplication(), { File(".") }, emptyMap())

    private fun props(id: String, partition: String? = "persist:a") = JSONObject()
        .put("src", "https://example.com/").put("id", id).put("partition", partition ?: JSONObject.NULL)
        .put("sandbox", JSONObject.NULL).put("slot", 1).put("generation", 1).put("session", 7).toString()

    private fun settle() = shadowOf(Looper.getMainLooper()).idle()

    /** A call as the package makes it; its answer, once given. */
    private fun call(op: String, vararg pairs: Pair<String, Any?>): Array<JSONObject?> {
        val answer = arrayOfNulls<JSONObject>(1)
        val args = JSONObject().put("id", "tab")
        for ((k, v) in pairs) args.put(k, v ?: JSONObject.NULL)
        FrameAutomations.run(op, args) { answer[0] = it }
        return answer
    }

    private fun Array<JSONObject?>.error() = this[0]?.optString("error")?.ifEmpty { null }

    /** A reply proxy that keeps what it was asked to run. */
    private class Proxy : JavaScriptReplyProxy() {
        val scripts = ArrayList<Pair<String, WebViewOutcomeReceiver<String, JavaScriptExecutionException>>>()
        override fun postMessage(message: String) {}
        override fun postMessage(message: ByteArray) {}
        override fun executeJavaScript(script: String, receiver: WebViewOutcomeReceiver<String, JavaScriptExecutionException>?) {
            scripts += script to receiver!!
        }
    }

    /** The world's script posting to its listener, as the main frame does. */
    private fun post(web: WebView, world: String, data: String, proxy: Proxy) = compat.worldListeners.getValue(web).getValue(world)
        .onPostMessage(web, WebMessageCompat(data), Uri.parse("https://example.com"), true, proxy)

    /** The last script [proxy] ran answers its value, as the world's script posts it. */
    private fun answer(web: WebView, proxy: Proxy, world: String, value: Any) {
        val (script, receiver) = proxy.scripts.last()
        receiver.onResult("true")
        val n = Regex("n: (\\d+)").find(script)!!.groupValues[1].toInt()
        post(web, world, JSONObject().put("n", n).put("r", value).toString(), proxy)
    }

    private class Peer(private val properties: Map<String, Any?> = emptyMap()) : InvocationHandler {
        private var peer: Any? = null
        override fun invoke(proxy: Any, method: Method, args: Array<out Any?>?): Any? {
            if (method.name != "getOrCreatePeer") return properties[method.name]
            return peer ?: (args!!.single() as Callable<*>).call().also { peer = it }
        }
    }

    private fun commit(web: WebView, url: String, beforeCommit: () -> Unit = {}) {
        val navigation = Navigation.forInvocationHandler(Peer(mapOf(
            "getUrl" to url, "getPage" to Peer(), "isSameDocument" to false, "wasInitiatedByPage" to true,
            "isHistory" to false, "isReload" to false, "didCommit" to true, "didCommitErrorPage" to false,
        )))
        val listener = compat.listeners.getValue(web)
        listener.onNavigationStarted(navigation)
        beforeCommit()
        listener.onNavigationCompleted(navigation)
    }

    @Test fun thePagesWorldAnswersWhatItCanWithoutAnythingOfExactsThere() {
        val natives = natives()
        natives.ensure(1, 1, 0, 0, props("tab"), 0)
        val web = compat.last()
        val title = call("evaluate", "expression" to "document.title")
        val shadow = shadowOf(web)
        assertTrue(shadow.lastEvaluatedJavascript.contains("document.title"))
        shadow.lastEvaluatedJavascriptCallback.onReceiveValue(JSONObject.quote("\"Auto\""))
        assertEquals("Auto", title[0]!!.getString("result"))
        val promise = call("evaluate", "expression" to "Promise.resolve(7)")
        shadow.lastEvaluatedJavascriptCallback.onReceiveValue("""{"__exactPromise":true}""")
        assertEquals("NotSupportedError", promise.error())
        assertTrue(promise[0]!!.getString("message").contains("sandbox"))
        val thrown = call("callFunction", "functionDeclaration" to "() => nope()", "arguments" to JSONArray())
        shadow.lastEvaluatedJavascriptCallback.onReceiveValue("""{"__exactThrown":true,"name":"ReferenceError","message":"nope is not defined"}""")
        assertEquals("ReferenceError", thrown.error())
        val unparsed = call("evaluate", "expression" to "1 +")
        shadow.lastEvaluatedJavascriptCallback.onReceiveValue("null")
        assertEquals("SyntaxError", unparsed.error())
        assertEquals("NotSupportedError", call("addPreloadScript", "functionDeclaration" to "() => {}").error())
        assertEquals("NotSupportedError", call("performActions", "actions" to JSONArray()).error())
        assertEquals("SyntaxError", call("evaluate", "expression" to "1", "sandbox" to "exact.browser").error())
        assertTrue("nothing of Exact's in the page's world", compat.pageWorld.isEmpty())
    }

    @Test fun aSandboxAnswersThroughItsWorldFromItsNextDocumentAndEndsWithIt() {
        // A name no earlier test gave: the app's names are kept, and a known one's world is on each new view.
        val sb = "t${System.nanoTime()}"
        val natives = natives()
        natives.ensure(1, 1, 0, 0, props("tab"), 0)
        val web = compat.last()
        assertEquals("InvalidStateError", call("evaluate", "expression" to "1", "sandbox" to sb).error())
        val added = call("addPreloadScript", "functionDeclaration" to "() => { window.isolated = 1 }", "sandbox" to sb)
        assertTrue(added[0]!!.getString("script").startsWith("script-"))
        assertEquals("the channel's post, then the preload", 2, compat.worldScripts.getValue(web).count { it.first == sb })
        val proxy = Proxy()
        post(web, sb, "ready", proxy)
        val seven = call("evaluate", "expression" to "new Promise(r => r(7))", "sandbox" to sb)
        answer(web, proxy, sb, "7")
        assertEquals(7, seven[0]!!.getInt("result"))
        val late = call("evaluate", "expression" to "Promise.reject(new TypeError('late'))", "sandbox" to sb)
        answer(web, proxy, sb, JSONObject().put("__exactThrown", true).put("name", "TypeError").put("message", "late"))
        assertEquals("TypeError", late.error())
        val unparsed = call("evaluate", "expression" to "1 +", "sandbox" to sb)
        proxy.scripts.last().second.onResult(null)
        assertEquals("SyntaxError", unparsed.error())

        val pending = call("evaluate", "expression" to "new Promise(() => {})", "sandbox" to sb)
        commit(web, "https://example.com/next")
        assertEquals("AbortError", pending.error())
        // The world was on the view before this document began: a call waits for its post, not the old proxy.
        val waits = call("evaluate", "expression" to "1", "sandbox" to sb)
        assertNull(waits[0])
        val second = Proxy()
        post(web, sb, "ready", second)
        answer(web, second, sb, "1")
        assertEquals(1, waits[0]!!.getInt("result"))
        assertEquals("the old document's proxy ran nothing more", 4, proxy.scripts.size)
        // A reply posted in another world is not this call's.
        call("addPreloadScript", "functionDeclaration" to "() => {}", "sandbox" to sb + "x")
        val other = call("evaluate", "expression" to "2", "sandbox" to sb)
        val n = Regex("n: (\\d+)").find(second.scripts.last().first)!!.groupValues[1].toInt()
        post(web, sb + "x", JSONObject().put("n", n).put("r", "3").toString(), second)
        assertNull(other[0])
        // The new document's post can reach the listener before WebView reports its commit.
        val early = Proxy()
        commit(web, "https://example.com/third") { post(web, sb, "ready", early) }
        assertEquals("AbortError", other.error())
        call("evaluate", "expression" to "1", "sandbox" to sb)
        assertEquals(1, early.scripts.size)

        // A call that acts takes the frame; its record says so.
        val acts = call("evaluate", "expression" to "1", "userActivation" to true)
        shadowOf(web).lastEvaluatedJavascriptCallback.onReceiveValue(JSONObject.quote("1"))
        assertNull(acts.error())
        settle()
        assertEquals("agent", NativesPartitionTest.Host.sent.last { it.event == "currententrychange" }.record.getString("controller"))

        // A changed partition is a new frame: a call in flight ends, and the preload script goes with it.
        post(web, sb, "ready", proxy)
        val held = call("evaluate", "expression" to "new Promise(() => {})", "sandbox" to sb)
        assertNull(held[0])
        natives.ensure(1, 1, 0, 0, props("tab", "persist:b"), 0)
        assertEquals("NotFoundError", held.error())
        val fresh = compat.last()
        assertNotSame(web, fresh)
        assertTrue(compat.worldScripts.getValue(fresh).any { it.first == sb && it.second.contains("window.isolated") })
        val kept = added[0]!!.getString("script")
        assertNull("the same id after the partition change", call("removePreloadScript", "script" to kept).error())
        assertFalse(compat.worldScripts.getValue(fresh).any { it.second.contains("window.isolated") })
        assertEquals("NotFoundError", call("removePreloadScript", "script" to kept).error())
    }

    @Test fun withoutWorldsEverySandboxIsRefusedAndNoFrameButAPartitionedOneIsDriven() {
        NativesPartitionTest.Features.worlds = false
        val natives = natives()
        natives.ensure(1, 1, 0, 0, props("tab"), 0)
        assertEquals("NotSupportedError", call("evaluate", "expression" to "1", "sandbox" to "t3").error())
        assertEquals("NotSupportedError", call("addPreloadScript", "functionDeclaration" to "() => {}", "sandbox" to "t3").error())
        assertTrue("no world was touched", compat.worldScripts.isEmpty() && compat.worldListeners.isEmpty())
        natives.ensure(2, 1, 0, 0, props("plain", partition = null).replace("\"slot\":1", "\"slot\":2"), 0)
        assertEquals("NotSupportedError", call("evaluate", "expression" to "1").let { a -> FrameAutomations.run("evaluate",
            JSONObject().put("id", "plain").put("expression", "1")) { a[0] = it }; a }.error())
        natives.ensure(3, 1, 0, 0, props("tab").replace("\"slot\":1", "\"slot\":3"), 0)
        assertEquals("two frames with one id", "NotFoundError", call("evaluate", "expression" to "1").error())
        assertEquals("NotFoundError", call("evaluate", "expression" to "1").let { a -> FrameAutomations.run("evaluate",
            JSONObject().put("id", "none").put("expression", "1")) { a[0] = it }; a }.error())
    }
}
