// @exact/browser-automation on Android (LLP 1020.002 D9, §11 "Android as built"): each call goes to the reader
// (`FrameAutomations`), which finds the app's live frame by its `id`. The runner has admitted the call under
// `browser.automation` before it reaches here.
package exact.browserautomation

import dev.exact.canvas.ExactExtension
import dev.exact.canvas.ExtensionContext
import dev.exact.canvas.FrameAutomations
import org.json.JSONObject

class BrowserAutomationModule(context: ExtensionContext) : ExactExtension(context) {
    override fun later(name: String, args: JSONObject, reply: (JSONObject) -> Unit) = FrameAutomations.run(name, args, reply)
}
