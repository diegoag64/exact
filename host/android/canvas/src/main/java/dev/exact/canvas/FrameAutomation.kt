package dev.exact.canvas

import android.annotation.SuppressLint
import android.view.MotionEvent
import android.webkit.WebView
import androidx.webkit.JavaScriptExecutionException
import androidx.webkit.JavaScriptExecutionWorld
import androidx.webkit.JavaScriptReplyProxy
import androidx.webkit.Page
import androidx.webkit.ScriptHandler
import androidx.webkit.WebMessageCompat
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import androidx.webkit.WebViewOutcomeReceiver
import org.json.JSONArray
import org.json.JSONObject
import org.json.JSONTokener

/**
 * `@exact/browser-automation` on one kept partitioned frame (LLP 1020.002 D9, §11 "Android as built"), owned by its
 * [FrameBrowser] so it outlives a web view a renderer's end replaces.
 *
 * Nothing of Exact's is ever in the page's own world (D1). Without a `sandbox`, `evaluate` and `callFunction` are
 * `WebView.evaluateJavascript` in the page, which answers synchronously: a promise to await there is
 * `NotSupportedError`, as is a preload script there, since WebView injects one into every frame and only a script of
 * Exact's would keep it to the main one. A `sandbox` is an AndroidX execution world (`JS_INJECTION_IN_FRAME_AND_WORLD`):
 * each world a call names gets a message listener of its own and a document-start script that posts to it, so each
 * document's [JavaScriptReplyProxy] runs the call in that world and its result comes back through the listener, which
 * exists only in that world. A world reaches a document only from its start, so a sandbox is ready in the documents
 * that start after the app first named it: the names are kept across launches ([named]), and each frame's view gets
 * every one's channel when it is made.
 */
internal class FrameAutomation(private val web: () -> WebView?, private val changed: () -> Unit) {
    /** `"agent"` after a call that acts, until a person's touch or key reaches the frame (D9). */
    var controller = "person"
        private set

    private class Preload(val id: String, val source: String, val sandbox: String)
    private val preloads = ArrayList<Preload>()

    /** What is installed on [view]: each preload's handler, each world, and each world's channel with the number of
     * navigations begun on the view before it was installed. */
    private var view: WebView? = null
    private val handlers = HashMap<String, ScriptHandler>()
    private val worlds = HashMap<String, JavaScriptExecutionWorld>()
    private val channels = HashMap<String, Int>()

    /** The document the frame shows, and each world's reply proxy in each document, by the page it posted from. */
    private var page: Page? = null
    private val proxies = HashMap<Pair<Page?, String>, JavaScriptReplyProxy>()
    /** Posts made while a navigation to a new document is uncommitted: only that document can have made them. */
    private var navigating = false
    private val early = HashMap<String, JavaScriptReplyProxy>()
    /** Navigations begun on the view, and how many had begun when the current document's began. */
    private var starts = 0
    private var documentStart = 0
    /** Calls waiting for a world's post from the current document, by world. */
    private val waiting = HashMap<String, ArrayList<(JavaScriptReplyProxy) -> Unit>>()

    private var nextId = 0
    /** Calls in flight: each one's answer, and the world it runs in (null: the page's). */
    private class Pending(val reply: (JSONObject) -> Unit, val world: String?)
    private val pending = HashMap<Int, Pending>()
    private val main = android.os.Handler(android.os.Looper.getMainLooper())

    init { live += this; attach() }

    // Lifetime

    /** Installs the preload scripts and every named world's channel on the frame's view, if it is a new one. */
    @SuppressLint("ClickableViewAccessibility")
    fun attach() {
        val current = web() ?: return
        if (current === view) return
        view = current
        forgetView()
        current.setOnTouchListener { _, event -> if (event.actionMasked == MotionEvent.ACTION_DOWN) personTook(); false }
        current.setOnKeyListener { _, _, event -> if (event.action == android.view.KeyEvent.ACTION_DOWN) personTook(); false }
        if (!ISOLATED) return
        for (name in named(current)) channel(current, name)
        for (p in preloads) install(current, p)
    }

    private fun forgetView() {
        handlers.clear(); worlds.clear(); channels.clear(); proxies.clear(); early.clear(); waiting.clear()
        page = null
        navigating = false
        starts = 0; documentStart = 0
    }

    fun starting() { navigating = true; early.clear(); starts++ }

    /** A new document committed (or one came back from the back-forward cache): a script in the old one is aborted. */
    fun committed(page: Page?) {
        this.page = page
        navigating = false
        documentStart = starts
        endAll("AbortError", "the document navigated away")
        for ((name, proxy) in early) ready(name, proxy)
        early.clear()
    }

    fun deleted(page: Page) { proxies.keys.removeAll { it.first == page } }

    /** The frame's view went (out of view, a renderer's end): its document with it. */
    fun detached() {
        view = null
        forgetView()
        endAll("AbortError", "the frame's page is gone")
    }

    /** The frame unmounted or changed its partition. */
    fun close() {
        live -= this
        view = null
        forgetView()
        endAll("NotFoundError", "the frame was removed or its partition changed")
    }

    /** A partition change makes a new frame: its preload scripts go with it, as on Apple. */
    fun adopt(old: FrameAutomation) {
        preloads += old.preloads
        nextId = maxOf(nextId, old.nextId)
        view?.let { v -> if (ISOLATED) preloads.forEach { install(v, it) } }
    }

    private fun endAll(error: String, message: String) {
        val ended = pending.values.toList()
        pending.clear()
        waiting.clear()
        ended.forEach { it.reply(failure(error, message)) }
    }

    private fun take() {
        if (controller == "agent") return
        controller = "agent"
        changed()
    }

    private fun personTook() {
        if (controller == "person") return
        controller = "person"
        changed()
    }

    // Calls

    fun run(op: String, args: JSONObject, reply: (JSONObject) -> Unit) {
        val sandbox = if (args.isNull("sandbox")) null else args.optString("sandbox")
        if (sandbox != null && (sandbox.isEmpty() || sandbox.startsWith("exact."))) {
            return reply(failure("SyntaxError", "${sandbox.ifEmpty { "an empty name" }} is not a sandbox: a name, not one beginning exact."))
        }
        if (sandbox != null && !ISOLATED) {
            return reply(failure("NotSupportedError", "$op: this device's WebView has no isolated worlds (JS_INJECTION_IN_FRAME_AND_WORLD), so a sandbox runs nowhere"))
        }
        // The preload scripts are the frame's, kept while it has no view and installed on the next.
        attach()
        when (op) {
            "addPreloadScript" -> {
                if (sandbox == null) return reply(failure("NotSupportedError", "addPreloadScript: Android's WebView runs a preload script in every frame of the page's world, and Exact puts nothing of its own there to keep it to the main one; name a sandbox"))
                val p = Preload("script-${++nextId}", args.optString("functionDeclaration"), sandbox)
                view?.let { name(it, sandbox); install(it, p) }
                preloads += p
                return reply(JSONObject().put("script", p.id))
            }
            "removePreloadScript" -> {
                val id = args.optString("script")
                val index = preloads.indexOfFirst { it.id == id }
                if (index < 0) return reply(failure("NotFoundError", "no preload script $id"))
                preloads.removeAt(index)
                handlers.remove(id)?.remove()
                return reply(JSONObject())
            }
        }
        val view = view ?: return reply(failure("InvalidStateError", "$op: the frame is out of view, with no page"))
        when (op) {
            "evaluate", "callFunction" -> script(op, args, sandbox, view, reply)
            else -> reply(failure("NotSupportedError", "$op is not offered on Android (LLP 1020.002 §11)"))
        }
    }

    private fun script(op: String, args: JSONObject, sandbox: String?, view: WebView, reply: (JSONObject) -> Unit) {
        if (args.optBoolean("userActivation", false)) take()
        val awaited = if (args.isNull("awaitPromise")) true else args.optBoolean("awaitPromise", true)
        val arguments = JSONObject.quote((args.optJSONArray("arguments") ?: JSONArray()).toString())
        val value = if (op == "evaluate") "(\n${args.optString("expression")}\n)"
            else "(\n${args.optString("functionDeclaration")}\n)(...JSON.parse($arguments))"
        // The expression runs in a function of its own, so no name of Exact's is in its scope; an exception is caught
        // and answered by its name, which the platform's own answer would lose.
        val ran = "(() => { try { return { v: $value }; } catch (e) { return { e }; } })()"
        val settle = when {
            sandbox == null && awaited -> "if (v && typeof v.then === 'function') return { __exactPromise: true };"
            sandbox == null -> "if (v && typeof v.then === 'function') return '{}';"
            awaited -> "v = await v;"
            else -> "if (v && typeof v.then === 'function') v = {};"
        }
        val text = "if ('e' in r) throw r.e; let v = r.v; $settle const t = JSON.stringify(v); return t === undefined ? 'null' : t;"
        if (sandbox != null) {
            try { name(view, sandbox) } catch (e: RuntimeException) { return reply(failure("NotSupportedError", "$op: the WebView refused: $e")) }
        }
        val number = ++nextId
        pending[number] = Pending(reply, sandbox)
        fun answer(result: JSONObject) { pending.remove(number)?.reply?.invoke(result) }
        if (sandbox == null) {
            // The page's world: the value as its JSON, or a promise, which only a script of Exact's could await there.
            val body = "((r) => { try { $text } catch (e) { $THROWN } })($ran)"
            return try { view.evaluateJavascript(body) { json -> answer(pageResult(json)) } }
                catch (e: RuntimeException) { answer(failure("NotSupportedError", "$op: the WebView refused: $e")) }
        }
        val body = "((post, r) => { (async () => { try { $text } catch (e) { $THROWN } })()" +
            ".then(x => post(JSON.stringify({ n: $number, r: x })), () => post(JSON.stringify({ n: $number, r: null })));" +
            " return true; })($CHANNEL.postMessage.bind($CHANNEL), $ran)"
        var sent = false
        fun send(proxy: JavaScriptReplyProxy) {
            if (number !in pending) return
            sent = true
            try { proxy.executeJavaScript(body, object : WebViewOutcomeReceiver<String, JavaScriptExecutionException> {
                override fun onResult(result: String?) { if (result != "true") answer(failure("SyntaxError", "the script does not parse")) }
                override fun onError(error: JavaScriptExecutionException) {
                    answer(if (error.errorType == JavaScriptExecutionException.ERROR_FRAME_DESTROYED) failure("AbortError", "the document navigated away")
                        else failure("Error", error.message ?: "the script did not run"))
                }
            }) } catch (e: RuntimeException) { answer(failure("NotSupportedError", "$op: the WebView refused: $e")) }
        }
        proxies[page to sandbox]?.let { return send(it) }
        // The world's channel was on the view before this document began, so its post is on its way.
        val installed = channels[sandbox]
        if (installed != null && (navigating || installed < documentStart)) {
            waiting.getOrPut(sandbox) { ArrayList() } += ::send
            main.postDelayed({ if (!sent) answer(failure("InvalidStateError", "$op: the document's sandbox $sandbox did not start")) }, READY_WAIT)
            return
        }
        answer(failure("InvalidStateError",
            "$op: sandbox $sandbox reaches this frame from its next document: Android's WebView runs a world's script only from a document's start"))
    }

    /** `evaluateJavascript`'s answer: the JSON of what the wrapper returned, or `null` where the script did not parse. */
    private fun pageResult(json: String?): JSONObject {
        val value = json?.let { runCatching { JSONTokener(it).nextValue() }.getOrNull() }
        return when {
            value is String -> settled(value)
            value is JSONObject && value.optBoolean("__exactPromise") -> failure("NotSupportedError",
                "awaiting a promise in the page's world needs a script of Exact's there; name a sandbox, or pass awaitPromise: false")
            value is JSONObject && value.optBoolean("__exactThrown") -> thrown(value)
            else -> failure("SyntaxError", "the script does not parse")
        }
    }

    // Worlds

    /** Notes a sandbox name for every frame: its channel is installed on each frame's view, now and when made. */
    private fun name(view: WebView, sandbox: String) {
        val names = named(view)
        if (sandbox !in names) {
            names += sandbox
            while (names.size > KEPT) names.remove(names.first())
            view.context.getSharedPreferences(PREFERENCES, android.content.Context.MODE_PRIVATE).edit()
                .putString("sandboxes", JSONArray(names.toList()).toString()).apply()
        }
        for (a in live) a.view?.let { a.channel(it, sandbox) }
    }

    private fun world(view: WebView, name: String) = worlds.getOrPut(name) { WebViewCompat.getExecutionWorld(view, name) }

    /** [name]'s listener, which only scripts in that world see, and the main frame's post to it at each document's start. */
    private fun channel(view: WebView, name: String) {
        if (name in channels) return
        val world = world(view, name)
        WebViewCompat.addWebMessageListener(view, CHANNEL, setOf("*"), world) { _, message, _, isMainFrame, proxy ->
            if (!isMainFrame || this.view !== view || message.type != WebMessageCompat.TYPE_STRING) return@addWebMessageListener
            val data = message.data ?: return@addWebMessageListener
            if (data == "ready") { if (navigating) early[name] = proxy else ready(name, proxy); return@addWebMessageListener }
            val posted = runCatching { JSONObject(data) }.getOrNull() ?: return@addWebMessageListener
            val n = posted.optInt("n")
            if (pending[n]?.world != name) return@addWebMessageListener
            val reply = pending.remove(n)!!.reply
            reply(when (val r = posted.opt("r")) {
                is String -> settled(r)
                is JSONObject -> thrown(r)
                else -> failure("Error", "the script's exception has no text")
            })
        }
        WebViewCompat.addJavaScriptOnEvent(view, "if (window === top) $CHANNEL.postMessage('ready');",
            WebViewCompat.INJECTION_EVENT_DOCUMENT_START, setOf("*"), world)
        channels[name] = starts
    }

    /** The current document's post in [name]'s world: its proxy, and the calls that waited for it. */
    private fun ready(name: String, proxy: JavaScriptReplyProxy) {
        proxies[page to name] = proxy
        waiting.remove(name)?.forEach { it(proxy) }
    }

    private fun install(view: WebView, p: Preload) {
        handlers[p.id] = WebViewCompat.addJavaScriptOnEvent(view, "if (window === top) (\n${p.source}\n)();",
            WebViewCompat.INJECTION_EVENT_DOCUMENT_START, setOf("*"), world(view, p.sandbox))
    }

    companion object {
        /** The listener's name, in each sandbox's world only. */
        private const val CHANNEL = "__exactAutomation"
        /** The exception `e` as a value, or `Error` where its name or text throws. */
        private const val THROWN = "try { return { __exactThrown: true, name: String(e?.name ?? 'Error'), message: String(e?.message ?? e) }; } " +
            "catch { return { __exactThrown: true, name: 'Error', message: '' }; }"
        /** How long a call waits for a document's world to start. */
        private const val READY_WAIT = 10_000L
        /** A result's JSON, at most this many bytes (Apple's limit). */
        private const val LIMIT = 64 * 1024
        private val ISOLATED get() = WebViewFeature.isFeatureSupported(WebViewFeature.JS_INJECTION_IN_FRAME_AND_WORLD)

        /** Every frame's automation. */
        private val live = LinkedHashSet<FrameAutomation>()

        /** The sandboxes the app has named, newest last, at most [KEPT], in its preferences across launches. */
        private var names: LinkedHashSet<String>? = null
        private const val PREFERENCES = "exact.browser-automation"
        private const val KEPT = 32
        private fun named(view: WebView): LinkedHashSet<String> = names ?: LinkedHashSet<String>().also { set ->
            val kept = view.context.getSharedPreferences(PREFERENCES, android.content.Context.MODE_PRIVATE).getString("sandboxes", null)
            kept?.let { runCatching { JSONArray(it) }.getOrNull() }?.let { a -> (0 until a.length()).mapTo(set) { a.optString(it) } }
            names = set
        }

        fun failure(error: String, message: String): JSONObject = JSONObject().put("error", error).put("message", message)

        private fun settled(text: String): JSONObject {
            val bytes = text.toByteArray(Charsets.UTF_8).size
            if (bytes > LIMIT) return failure("QuotaExceededError", "the result is $bytes bytes of JSON; at most $LIMIT")
            return JSONObject().put("result", runCatching { JSONTokener(text).nextValue() }.getOrNull() ?: JSONObject.NULL)
        }

        /** The guest's exception by its own name (`TypeError`), else `Error`, as on Apple. */
        private fun thrown(value: JSONObject): JSONObject {
            val name = value.optString("name")
            return failure(if (name.endsWith("Error") && ' ' !in name) name else "Error", value.optString("message"))
        }
    }
}

/**
 * `@exact/browser-automation`'s calls on Android, for its implementation, on the main thread: the one live iframe of
 * this app with the call's `id` (LLP 1020.002 D9). A frame without a kept partition answers `NotSupportedError`, as on
 * Apple; none, or more than one (two sessions mounting the same id), `NotFoundError`.
 */
object FrameAutomations {
    /** Every reader, each with its frames. */
    internal val readers: MutableSet<Natives> = java.util.Collections.newSetFromMap(java.util.WeakHashMap())

    @JvmStatic fun run(op: String, args: JSONObject, reply: (JSONObject) -> Unit) {
        val id = args.optString("id")
        val frames = readers.flatMap { it.automations(id) }
        if (frames.size != 1) return reply(FrameAutomation.failure("NotFoundError", "no unique live iframe with id $id"))
        val automation = frames[0] ?: return reply(FrameAutomation.failure("NotSupportedError",
            "$op: iframe $id has no partition; only a partitioned frame is driven"))
        try {
            automation.run(op, args, reply)
        } catch (e: RuntimeException) {
            // The WebView provider refused (an unsupported operation, a world it will not make).
            reply(FrameAutomation.failure("NotSupportedError", "$op: the WebView refused: $e"))
        }
    }
}
