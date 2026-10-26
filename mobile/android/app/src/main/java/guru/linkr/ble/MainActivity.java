package guru.linkr.ble;

import android.os.Bundle;

import com.getcapacitor.BridgeActivity;

import guru.linkr.ble.mcp.LinkrMcpPlugin;

public class MainActivity extends BridgeActivity {

    @Override
    public void onCreate(Bundle savedInstanceState) {
        // Local plugins must be registered before the bridge is created.
        registerPlugin(LinkrMcpPlugin.class);
        super.onCreate(savedInstanceState);
    }
}
