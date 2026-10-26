package guru.linkr.ble.mcp;

import android.content.Context;
import android.content.SharedPreferences;
import android.os.Handler;
import android.os.Looper;
import com.getcapacitor.JSObject;
import java.io.IOException;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.NetworkInterface;
import java.net.SocketException;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.Enumeration;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ThreadFactory;
import java.util.concurrent.TimeUnit;
import org.json.JSONArray;
import org.json.JSONObject;

/**
 * Owns the MCP server lifecycle: persisted configuration and bearer token, the
 * running {@link McpHttpServer}, forwarded requests waiting on the WebView, and
 * the state object shared with the UI.
 *
 * <p>This class is the only place that touches Android storage; the transport
 * itself stays platform-free.
 */
public final class McpServerManager {

    private static final String PREFS_NAME = "linkr_mcp";
    private static final String KEY_TOKEN = "token";
    private static final String KEY_PORT = "port";
    private static final String KEY_BIND = "bind";

    public static final int DEFAULT_PORT = 8712;
    public static final String BIND_LOOPBACK = "loopback";
    public static final String BIND_LAN = "lan";

    private static final int MIN_PORT = 1024;
    private static final int MAX_PORT = 65535;
    private static final long RESPONSE_TIMEOUT_MS = 180_000L;
    private static final int TOKEN_BYTES = 32;

    public static final String TIMEOUT_BODY =
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32000,\"message\":\"Linkr Bee app did not answer in time\"}}";

    private static final McpServerManager INSTANCE = new McpServerManager();

    public static McpServerManager getInstance(Context context) {
        INSTANCE.ensureContext(context);
        return INSTANCE;
    }

    private final Object lock = new Object();
    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private final ConcurrentHashMap<String, Pending> pending = new ConcurrentHashMap<>();
    private final SecureRandom random = new SecureRandom();
    private final ScheduledExecutorService scheduler;

    private Context appContext;
    private SharedPreferences prefs;
    private LinkrMcpPlugin plugin;
    private McpHttpServer server;
    private String token;
    private int port = DEFAULT_PORT;
    private String bind = BIND_LAN;
    private boolean foreground;
    private volatile String lastError;

    private McpServerManager() {
        scheduler = Executors.newSingleThreadScheduledExecutor(new ThreadFactory() {
            @Override
            public Thread newThread(Runnable runnable) {
                Thread thread = new Thread(runnable, "linkr-mcp-timeout");
                thread.setDaemon(true);
                return thread;
            }
        });
    }

    private void ensureContext(Context context) {
        if (context == null) {
            return;
        }
        Context application = context.getApplicationContext();
        if (application == null) {
            application = context;
        }
        synchronized (lock) {
            if (prefs != null) {
                return;
            }
            appContext = application;
            prefs = application.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE);
            token = prefs.getString(KEY_TOKEN, null);
            port = prefs.getInt(KEY_PORT, DEFAULT_PORT);
            bind = prefs.getString(KEY_BIND, BIND_LAN);
            if (token == null || !token.matches("[0-9a-f]{64}")) {
                token = newToken();
                prefs.edit().putString(KEY_TOKEN, token).apply();
            }
            if (!BIND_LOOPBACK.equals(bind) && !BIND_LAN.equals(bind)) {
                bind = BIND_LAN;
            }
            if (port != 0 && (port < MIN_PORT || port > MAX_PORT)) {
                port = DEFAULT_PORT;
            }
        }
    }

    public void attach(LinkrMcpPlugin plugin) {
        synchronized (lock) {
            this.plugin = plugin;
        }
        emitStateChanged();
    }

    public void detach(LinkrMcpPlugin plugin) {
        synchronized (lock) {
            if (this.plugin == plugin) {
                this.plugin = null;
            }
        }
        failAllPending(503, McpHttpServer.NOT_READY_BODY);
    }

    public void setForeground(boolean foreground) {
        boolean changed;
        synchronized (lock) {
            changed = this.foreground != foreground;
            this.foreground = foreground;
        }
        if (changed) {
            emitStateChanged();
        }
    }

    /** Stops the server and the foreground service when the activity is destroyed. */
    public void onActivityDestroyed() {
        synchronized (lock) {
            foreground = false;
        }
        stop();
    }

    /**
     * Starts (or restarts) the server. The port may be 0 for an ephemeral bind;
     * otherwise it must be 1024–65535. A null port/bind falls back to the
     * persisted configuration.
     */
    public void start(Integer requestedPort, String requestedBind) throws IOException {
        ensureContext(appContext);
        int configuredPort;
        String configuredBind;
        synchronized (lock) {
            configuredPort = requestedPort == null ? port : requestedPort;
            configuredBind = requestedBind == null ? bind : requestedBind;
        }
        if (configuredPort != 0 && (configuredPort < MIN_PORT || configuredPort > MAX_PORT)) {
            throw new IllegalArgumentException("Port must be 0 or between 1024 and 65535");
        }
        if (!BIND_LOOPBACK.equals(configuredBind) && !BIND_LAN.equals(configuredBind)) {
            throw new IllegalArgumentException("Bind must be \"loopback\" or \"lan\"");
        }

        McpHttpServer previous;
        synchronized (lock) {
            previous = server;
            server = null;
        }
        if (previous != null) {
            // Release the old socket before binding the new configuration so a
            // restart on the same port cannot collide with ourselves.
            previous.stop();
        }
        failAllPending(503, McpHttpServer.NOT_READY_BODY);

        McpHttpServer next = new McpHttpServer(this::handleRequest, getToken(), BIND_LAN.equals(configuredBind));
        try {
            next.start(configuredPort);
        } catch (IOException error) {
            synchronized (lock) {
                lastError = error.getMessage() == null ? "Could not start the MCP server" : error.getMessage();
                if (prefs != null) {
                    prefs.edit().putInt(KEY_PORT, configuredPort).putString(KEY_BIND, configuredBind).apply();
                }
                port = configuredPort;
                bind = configuredBind;
            }
            /* A restart can fail after the old listener was closed (for example
             * when the user picks a busy port). The foreground service from the
             * previous start would otherwise keep claiming the server is
             * listening, so tear it down with the socket. */
            Context context = appContext;
            if (context != null) {
                LinkrMcpService.stop(context);
            }
            emitStateChanged();
            throw error;
        }

        synchronized (lock) {
            server = next;
            port = configuredPort;
            bind = configuredBind;
            lastError = null;
            if (prefs != null) {
                prefs.edit().putInt(KEY_PORT, configuredPort).putString(KEY_BIND, configuredBind).apply();
            }
        }
        Context context = appContext;
        if (context != null) {
            try {
                LinkrMcpService.start(context, next.getPort());
            } catch (RuntimeException error) {
                /* Android 12+ can refuse a foreground-service start when the app
                 * is not in a foreground state. The socket is already bound, so
                 * keep the server usable for the current session and surface the
                 * weaker lifecycle guarantee instead of failing the start. */
                synchronized (lock) {
                    lastError = error.getMessage() == null
                            ? "The MCP server is running without a foreground service"
                            : "The MCP server is running without a foreground service: " + error.getMessage();
                }
            }
        }
        emitStateChanged();
    }

    public void stop() {
        McpHttpServer current;
        synchronized (lock) {
            current = server;
            server = null;
        }
        if (current != null) {
            current.stop();
        }
        failAllPending(503, McpHttpServer.NOT_READY_BODY);
        Context context = appContext;
        if (context != null) {
            LinkrMcpService.stop(context);
        }
        emitStateChanged();
    }

    public JSObject regenerateToken() {
        String rotated = newToken();
        McpHttpServer current;
        synchronized (lock) {
            token = rotated;
            if (prefs != null) {
                prefs.edit().putString(KEY_TOKEN, rotated).apply();
            }
            current = server;
        }
        if (current != null) {
            current.setToken(rotated);
        }
        emitStateChanged();
        return getState();
    }

    public JSObject getState() {
        McpHttpServer current;
        String stateBind;
        String stateToken;
        boolean stateForeground;
        String error;
        int configuredPort;
        synchronized (lock) {
            current = server;
            stateBind = bind;
            stateToken = token == null ? "" : token;
            stateForeground = foreground;
            error = lastError;
            configuredPort = port;
        }
        boolean running = current != null && current.isRunning();
        int boundPort = running ? current.getPort() : configuredPort;
        List<String> urls = buildUrls(boundPort, stateBind);

        JSObject state = new JSObject();
        state.put("running", running);
        state.put("port", boundPort);
        state.put("bind", stateBind);
        state.put("token", stateToken);
        state.put("url", preferredUrl(boundPort, stateBind, urls));
        state.put("urls", new JSONArray(urls));
        state.put("foreground", stateForeground);
        state.put("activeRequests", pending.size());
        state.put("lastError", error == null ? JSONObject.NULL : error);
        return state;
    }

    /** Called by the transport for every authenticated POST /mcp body. */
    private CompletableFuture<McpHttpServer.Response> handleRequest(McpHttpServer.Request request) {
        LinkrMcpPlugin target;
        synchronized (lock) {
            target = plugin;
        }
        if (target == null) {
            return CompletableFuture.completedFuture(
                    new McpHttpServer.Response(503, McpHttpServer.NOT_READY_BODY));
        }

        String id = UUID.randomUUID().toString();
        CompletableFuture<McpHttpServer.Response> future = new CompletableFuture<>();
        Pending entry = new Pending(future);
        pending.put(id, entry);
        entry.timeout = scheduler.schedule(() -> {
            if (pending.remove(id, entry)) {
                future.complete(new McpHttpServer.Response(504, TIMEOUT_BODY));
            }
        }, RESPONSE_TIMEOUT_MS, TimeUnit.MILLISECONDS);
        future.whenComplete((response, error) -> pending.remove(id, entry));

        JSObject payload = new JSObject();
        payload.put("id", id);
        payload.put("method", request.method);
        payload.put("path", request.path);
        payload.put("body", request.body);
        payload.put("remote", request.remote);
        payload.put("userAgent", request.userAgent);

        boolean posted = mainHandler.post(() -> {
            LinkrMcpPlugin current;
            synchronized (lock) {
                current = plugin;
            }
            if (current == null || !current.hasHttpRequestListener()) {
                if (pending.remove(id, entry)) {
                    entry.cancelTimeout();
                    future.complete(new McpHttpServer.Response(503, McpHttpServer.NOT_READY_BODY));
                }
                return;
            }
            try {
                current.emitHttpRequest(payload);
            } catch (RuntimeException error) {
                if (pending.remove(id, entry)) {
                    entry.cancelTimeout();
                    future.complete(new McpHttpServer.Response(503, McpHttpServer.NOT_READY_BODY));
                }
            }
        });
        if (!posted) {
            pending.remove(id, entry);
            entry.cancelTimeout();
            return CompletableFuture.completedFuture(
                    new McpHttpServer.Response(503, McpHttpServer.NOT_READY_BODY));
        }
        return future;
    }

    /** Completes a forwarded request. Unknown or already answered ids are ignored. */
    public void respond(String id, int status, String body) {
        if (id == null) {
            return;
        }
        Pending entry = pending.remove(id);
        if (entry == null) {
            return;
        }
        entry.cancelTimeout();
        entry.future.complete(new McpHttpServer.Response(status, body == null ? "" : body));
    }

    public void emitStateChanged() {
        mainHandler.post(() -> {
            LinkrMcpPlugin current;
            synchronized (lock) {
                current = plugin;
            }
            if (current != null) {
                current.emitStateChanged(getState());
            }
        });
    }

    /**
     * Called by {@link LinkrMcpService} when Android refuses to promote it to a
     * foreground service (a missing runtime permission or a background start).
     * The listener keeps running while the activity lives, so the failure is
     * reported as a weaker lifecycle guarantee instead of crashing the app on a
     * settings toggle.
     */
    public void reportServiceFailure(String message) {
        synchronized (lock) {
            lastError = message == null || message.isEmpty()
                    ? "The MCP server is running without a foreground service"
                    : message;
        }
        emitStateChanged();
    }

    private void failAllPending(int status, String body) {
        for (String id : new ArrayList<>(pending.keySet())) {
            Pending entry = pending.remove(id);
            if (entry != null) {
                entry.cancelTimeout();
                entry.future.complete(new McpHttpServer.Response(status, body));
            }
        }
    }

    private String getToken() {
        synchronized (lock) {
            if (token == null || token.isEmpty()) {
                token = newToken();
                if (prefs != null) {
                    prefs.edit().putString(KEY_TOKEN, token).apply();
                }
            }
            return token;
        }
    }

    private String newToken() {
        byte[] bytes = new byte[TOKEN_BYTES];
        random.nextBytes(bytes);
        StringBuilder hex = new StringBuilder(TOKEN_BYTES * 2);
        for (byte value : bytes) {
            hex.append(String.format(Locale.ROOT, "%02x", value & 0xff));
        }
        return hex.toString();
    }

    private List<String> buildUrls(int boundPort, String stateBind) {
        Set<String> loopback = new LinkedHashSet<>();
        loopback.add("http://127.0.0.1:" + boundPort + "/mcp");
        if (BIND_LOOPBACK.equals(stateBind)) {
            return new ArrayList<>(loopback);
        }
        Set<String> urls = new LinkedHashSet<>(loopback);
        for (String address : lanAddresses()) {
            urls.add("http://" + address + ":" + boundPort + "/mcp");
        }
        return new ArrayList<>(urls);
    }

    private String preferredUrl(int boundPort, String stateBind, List<String> urls) {
        if (!BIND_LOOPBACK.equals(stateBind)) {
            for (String url : urls) {
                if (!url.contains("127.0.0.1")) {
                    return url;
                }
            }
        }
        return urls.isEmpty() ? "" : urls.get(0);
    }

    /**
     * Candidate base URLs, best first: an interface Android most likely routes
     * LAN traffic over (Wi-Fi, Ethernet, hotspot) before any other, and IPv4
     * before IPv6. Cellular and VPN interfaces are excluded: they are up and
     * look like ordinary interfaces to {@link NetworkInterface}, but a client on
     * the same Wi-Fi network cannot reach a carrier NAT or tunnel address.
     */
    private List<String> lanAddresses() {
        List<AddressCandidate> candidates = new ArrayList<>();
        try {
            Enumeration<NetworkInterface> interfaces = NetworkInterface.getNetworkInterfaces();
            while (interfaces != null && interfaces.hasMoreElements()) {
                NetworkInterface network = interfaces.nextElement();
                if (!network.isUp() || network.isLoopback() || network.isVirtual()) {
                    continue;
                }
                String name = network.getName() == null ? "" : network.getName().toLowerCase(Locale.ROOT);
                int interfaceRank = interfaceRank(name);
                if (interfaceRank < 0) {
                    continue;
                }
                Enumeration<InetAddress> inetAddresses = network.getInetAddresses();
                while (inetAddresses.hasMoreElements()) {
                    InetAddress address = inetAddresses.nextElement();
                    if (address.isLoopbackAddress() || address.isLinkLocalAddress() || address.isMulticastAddress()) {
                        continue;
                    }
                    boolean ipv4 = address instanceof Inet4Address;
                    String host = ipv4 ? address.getHostAddress() : "[" + address.getHostAddress() + "]";
                    candidates.add(new AddressCandidate(host, interfaceRank * 10 + (ipv4 ? 1 : 0)));
                }
            }
        } catch (SocketException ignored) {
            // No usable interface: loopback stays the only address.
        }
        candidates.sort((left, right) -> Integer.compare(right.score, left.score));
        List<String> addresses = new ArrayList<>();
        for (AddressCandidate candidate : candidates) {
            addresses.add(candidate.host);
        }
        return addresses;
    }

    /** 2 for a LAN-grade interface, 1 for another usable one, -1 to exclude it. */
    private static int interfaceRank(String name) {
        if (name.isEmpty()) {
            return 1;
        }
        if (name.startsWith("rmnet") || name.startsWith("ccmni") || name.startsWith("pdp")
                || name.startsWith("tun") || name.startsWith("tap") || name.startsWith("ppp")
                || name.startsWith("dummy") || name.startsWith("sit") || name.startsWith("ip6tnl")) {
            return -1;
        }
        if (name.startsWith("wlan") || name.startsWith("wifi") || name.startsWith("eth")
                || name.startsWith("ap") || name.startsWith("rndis") || name.startsWith("usb")) {
            return 2;
        }
        return 1;
    }

    private static final class AddressCandidate {

        final String host;
        final int score;

        AddressCandidate(String host, int score) {
            this.host = host;
            this.score = score;
        }
    }

    private static final class Pending {

        final CompletableFuture<McpHttpServer.Response> future;
        volatile ScheduledFuture<?> timeout;

        Pending(CompletableFuture<McpHttpServer.Response> future) {
            this.future = future;
        }

        void cancelTimeout() {
            ScheduledFuture<?> scheduled = timeout;
            if (scheduled != null) {
                scheduled.cancel(false);
            }
        }
    }
}
