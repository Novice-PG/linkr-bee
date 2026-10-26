package guru.linkr.ble.mcp;

import com.getcapacitor.JSObject;
import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.CapacitorPlugin;

/**
 * Capacitor bridge for the native MCP server.
 *
 * <p>Java only authenticates and transports; the WebView answers every JSON-RPC
 * request through the {@code httpRequest} event and {@code respond} call.
 */
@CapacitorPlugin(name = "LinkrMcp")
public class LinkrMcpPlugin extends Plugin {

    private McpServerManager manager;

    @Override
    public void load() {
        manager = McpServerManager.getInstance(getContext());
        manager.attach(this);
    }

    @Override
    protected void handleOnResume() {
        if (manager != null) {
            manager.setForeground(true);
        }
    }

    @Override
    protected void handleOnPause() {
        if (manager != null) {
            manager.setForeground(false);
        }
    }

    @Override
    protected void handleOnDestroy() {
        if (manager != null) {
            manager.onActivityDestroyed();
            manager.detach(this);
        }
    }

    @PluginMethod
    public void start(PluginCall call) {
        if (manager == null) {
            call.reject("MCP bridge is not ready");
            return;
        }
        Integer port = call.getInt("port");
        String bind = call.getString("bind");
        try {
            manager.start(port, bind);
            call.resolve(manager.getState());
        } catch (Exception error) {
            String message = error.getMessage() == null ? "Could not start the MCP server" : error.getMessage();
            call.reject(message, error);
        }
    }

    @PluginMethod
    public void stop(PluginCall call) {
        if (manager == null) {
            call.reject("MCP bridge is not ready");
            return;
        }
        manager.stop();
        call.resolve(manager.getState());
    }

    @PluginMethod
    public void getState(PluginCall call) {
        if (manager == null) {
            call.reject("MCP bridge is not ready");
            return;
        }
        call.resolve(manager.getState());
    }

    @PluginMethod
    public void regenerateToken(PluginCall call) {
        if (manager == null) {
            call.reject("MCP bridge is not ready");
            return;
        }
        call.resolve(manager.regenerateToken());
    }

    @PluginMethod
    public void respond(PluginCall call) {
        if (manager == null) {
            call.reject("MCP bridge is not ready");
            return;
        }
        String id = call.getString("id");
        if (id == null) {
            call.reject("Request id is required");
            return;
        }
        Integer status = call.getInt("status");
        String body = call.getString("body");
        manager.respond(id, status == null ? 200 : status, body == null ? "" : body);
        call.resolve();
    }

    /** True while the WebView has subscribed to forwarded HTTP requests. */
    public boolean hasHttpRequestListener() {
        return hasListeners("httpRequest");
    }

    public void emitHttpRequest(JSObject payload) {
        notifyListeners("httpRequest", payload);
    }

    public void emitStateChanged(JSObject state) {
        notifyListeners("stateChanged", state);
    }
}
