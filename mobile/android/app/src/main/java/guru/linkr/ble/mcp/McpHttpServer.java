package guru.linkr.ble.mcp;

import java.io.BufferedInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.LinkedHashMap;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.ThreadFactory;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Small authenticated HTTP server that fronts the app's MCP endpoint.
 *
 * <p>The class deliberately contains no Android imports so the transport can be
 * exercised on a host JVM. All MCP protocol logic lives in JavaScript; this
 * server only parses an HTTP request, enforces the security envelope (origin,
 * bearer token, size limits) and hands authenticated {@code POST /mcp} bodies
 * to a {@link Handler}. The handler answers with a {@link Response} which is
 * written back verbatim.
 */
public final class McpHttpServer {

    /** Request line plus headers may not exceed this many bytes. */
    public static final int MAX_HEADER_BYTES = 16 * 1024;

    /** Content-Length is rejected above this size (1 MiB). */
    public static final int MAX_BODY_BYTES = 1024 * 1024;

    /** Socket read timeout for the request line, headers and body. */
    public static final int READ_TIMEOUT_MS = 10_000;

    /** Maximum number of requests parsed concurrently. */
    public static final int MAX_CONCURRENT_REQUESTS = 8;

    /** Answer used when the WebView side cannot serve the request. */
    public static final String NOT_READY_BODY =
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32000,\"message\":\"Linkr Bee app is not ready\"}}";

    private static final String STATUS_BODY = "{\"name\":\"linkr-bee-mcp\",\"version\":\"0.1.0\",\"mcp\":\"/mcp\"}";
    private static final String BAD_REQUEST_BODY = "{\"error\":\"bad request\"}";
    private static final String UNAUTHORIZED_BODY = "{\"error\":\"unauthorized\"}";
    private static final String FORBIDDEN_BODY = "{\"error\":\"origin not allowed\"}";
    private static final String NOT_FOUND_BODY = "{\"error\":\"not found\"}";
    private static final String METHOD_NOT_ALLOWED_BODY = "{\"error\":\"method not allowed\"}";
    private static final String LENGTH_REQUIRED_BODY = "{\"error\":\"content length required\"}";
    private static final String TOO_LARGE_BODY = "{\"error\":\"request body too large\"}";

    private static final int MAX_QUEUED_REQUESTS = 64;

    /** Authenticated request, as seen by the JS sink. */
    public static final class Request {

        public final String method;
        public final String path;
        public final String remote;
        public final String userAgent;
        public final String body;

        public Request(String method, String path, String remote, String userAgent, String body) {
            this.method = method;
            this.path = path;
            this.remote = remote;
            this.userAgent = userAgent;
            this.body = body;
        }
    }

    /** Response produced by the JS sink. */
    public static final class Response {

        public final int status;
        public final String body;

        public Response(int status, String body) {
            this.status = status;
            this.body = body;
        }
    }

    /** Bridge to the WebView. Only authenticated {@code POST /mcp} bodies reach it. */
    public interface Handler {
        CompletableFuture<Response> handle(Request request);
    }

    private final Handler handler;
    private final boolean bindLan;
    private final Set<CompletableFuture<Response>> inFlight = ConcurrentHashMap.newKeySet();

    private volatile String token;
    private volatile ServerSocket serverSocket;
    private volatile int port = -1;
    private volatile boolean running;
    private volatile ThreadPoolExecutor executor;

    public McpHttpServer(Handler handler, String token, boolean bindLan) {
        this.handler = handler;
        this.token = token;
        this.bindLan = bindLan;
    }

    /** Rotates the bearer token without restarting the server. */
    public void setToken(String token) {
        this.token = token;
    }

    /**
     * Binds and starts accepting. Pass {@code 0} for an ephemeral port (tests).
     *
     * @return the port actually bound
     */
    public synchronized int start(int requestedPort) throws IOException {
        if (requestedPort < 0 || requestedPort > 65535) {
            throw new IllegalArgumentException("Port must be between 0 and 65535");
        }
        if (running) {
            return port;
        }
        ServerSocket socket = new ServerSocket();
        socket.setReuseAddress(true);
        InetAddress address = bindLan ? InetAddress.getByName("0.0.0.0") : InetAddress.getLoopbackAddress();
        try {
            socket.bind(new InetSocketAddress(address, requestedPort), 50);
        } catch (IOException error) {
            /* A failed bind must not leak the descriptor: a retry loop on a busy
             * port would otherwise run the app out of file descriptors. */
            try {
                socket.close();
            } catch (IOException ignored) {
                // Nothing was bound; the original bind error is what matters.
            }
            throw error;
        }
        serverSocket = socket;
        port = socket.getLocalPort();

        AtomicInteger workerIndex = new AtomicInteger();
        ThreadFactory threadFactory = runnable -> {
            Thread thread = new Thread(runnable, "linkr-mcp-worker-" + workerIndex.incrementAndGet());
            thread.setDaemon(true);
            return thread;
        };
        executor = new ThreadPoolExecutor(
                MAX_CONCURRENT_REQUESTS,
                MAX_CONCURRENT_REQUESTS,
                30L,
                TimeUnit.SECONDS,
                new ArrayBlockingQueue<>(MAX_QUEUED_REQUESTS),
                threadFactory,
                new ThreadPoolExecutor.AbortPolicy());
        running = true;

        Thread acceptThread = new Thread(this::acceptLoop, "linkr-mcp-accept");
        acceptThread.setDaemon(true);
        acceptThread.start();
        return port;
    }

    /** Stops accepting, answers in-flight requests with 503 and closes the socket. Idempotent. */
    public synchronized void stop() {
        if (!running && serverSocket == null) {
            return;
        }
        running = false;
        for (CompletableFuture<Response> delivery : inFlight) {
            delivery.complete(new Response(503, NOT_READY_BODY));
        }
        inFlight.clear();
        ServerSocket socket = serverSocket;
        serverSocket = null;
        if (socket != null) {
            try {
                socket.close();
            } catch (IOException ignored) {
                // The listener is being torn down; there is nothing to report.
            }
        }
        ThreadPoolExecutor pool = executor;
        executor = null;
        if (pool != null) {
            /* Queued tasks never reach handleConnection's try-with-resources.
             * Close their sockets explicitly when discarding the queue. */
            for (Runnable queued : pool.shutdownNow()) {
                if (queued instanceof ClientTask) ((ClientTask) queued).close();
            }
        }
    }

    public boolean isRunning() {
        return running;
    }

    /** The bound port, or -1 before {@link #start(int)}. */
    public int getPort() {
        return port;
    }

    private void acceptLoop() {
        while (running) {
            ServerSocket socket = serverSocket;
            if (socket == null) {
                return;
            }
            Socket client;
            try {
                client = socket.accept();
            } catch (IOException error) {
                return;
            }
            try {
                client.setSoTimeout(READ_TIMEOUT_MS);
                ThreadPoolExecutor pool = executor;
                if (pool == null) {
                    closeQuietly(client);
                    return;
                }
                pool.execute(new ClientTask(client));
            } catch (RejectedExecutionException rejected) {
                try {
                    writeResponse(client, new Response(503, NOT_READY_BODY), null);
                } catch (IOException ignored) {
                    // Client already gone.
                } finally {
                    closeQuietly(client);
                }
            } catch (IOException ignored) {
                closeQuietly(client);
            }
        }
    }

    private final class ClientTask implements Runnable {
        private final Socket socket;

        ClientTask(Socket socket) {
            this.socket = socket;
        }

        @Override
        public void run() {
            handleConnection(socket);
        }

        void close() {
            closeQuietly(socket);
        }
    }

    private void handleConnection(Socket client) {
        try (Socket socket = client) {
            socket.setSoTimeout(READ_TIMEOUT_MS);
            InputStream input = new BufferedInputStream(socket.getInputStream());
            LineReader reader = new LineReader(input, MAX_HEADER_BYTES);

            String requestLine = reader.readLine();
            if (requestLine == null) {
                if (reader.overflowed()) {
                    writeResponse(socket, new Response(400, BAD_REQUEST_BODY), null);
                }
                return;
            }
            String[] parts = requestLine.split(" ", 3);
            if (parts.length < 3 || parts[0].isEmpty() || parts[1].isEmpty()) {
                writeResponse(socket, new Response(400, BAD_REQUEST_BODY), null);
                return;
            }
            String method = parts[0];
            String target = parts[1];

            Map<String, String> headers = new LinkedHashMap<>();
            String line;
            while ((line = reader.readLine()) != null && !line.isEmpty()) {
                int colon = line.indexOf(':');
                if (colon > 0) {
                    headers.put(
                            line.substring(0, colon).trim().toLowerCase(Locale.ROOT),
                            line.substring(colon + 1).trim());
                }
            }
            if (reader.overflowed()) {
                writeResponse(socket, new Response(400, BAD_REQUEST_BODY), null);
                return;
            }
            if (line == null) {
                // Connection closed halfway through the headers: nothing to answer.
                return;
            }

            String path = target;
            int query = path.indexOf('?');
            if (query >= 0) {
                path = path.substring(0, query);
            }
            boolean hasOrigin = headers.containsKey("origin");

            if ("OPTIONS".equals(method)) {
                if (hasOrigin) {
                    writeResponse(socket, new Response(403, FORBIDDEN_BODY), null);
                } else {
                    writeResponse(socket, new Response(204, ""), null);
                }
                return;
            }

            if ("POST".equals(method) && "/mcp".equals(path)) {
                Response response = handleMcpPost(socket, input, headers, hasOrigin);
                if (response != null) {
                    writeResponse(socket, response, null);
                }
                return;
            }

            if ("GET".equals(method) && "/".equals(path)) {
                writeResponse(socket, new Response(200, STATUS_BODY), null);
                return;
            }

            if (("GET".equals(method) || "DELETE".equals(method)) && "/mcp".equals(path)) {
                writeResponse(socket, new Response(405, METHOD_NOT_ALLOWED_BODY), "POST");
                return;
            }

            writeResponse(socket, new Response(404, NOT_FOUND_BODY), null);
        } catch (IOException ignored) {
            // Read timeout, abrupt disconnect or shutdown; the socket is closed below.
        }
    }

    /** Returns a response, or null when the connection died while reading the body. */
    private Response handleMcpPost(Socket socket, InputStream input, Map<String, String> headers, boolean hasOrigin)
            throws IOException {
        if (hasOrigin) {
            return new Response(403, FORBIDDEN_BODY);
        }
        if (!isAuthorized(headers.get("authorization"))) {
            return new Response(401, UNAUTHORIZED_BODY);
        }
        String transferEncoding = headers.get("transfer-encoding");
        if (transferEncoding != null && transferEncoding.toLowerCase(Locale.ROOT).contains("chunked")) {
            return new Response(411, LENGTH_REQUIRED_BODY);
        }
        String contentLength = headers.get("content-length");
        if (contentLength == null || contentLength.trim().isEmpty()) {
            return new Response(411, LENGTH_REQUIRED_BODY);
        }
        /* RFC 9110 allows only 1*DIGIT. Accepting signs or whitespace here would
         * let a proxy and this server disagree about where a body starts. */
        String rawLength = contentLength.trim();
        if (!rawLength.matches("[0-9]+")) {
            return new Response(400, BAD_REQUEST_BODY);
        }
        long length;
        try {
            length = Long.parseLong(rawLength);
        } catch (NumberFormatException invalid) {
            return new Response(400, BAD_REQUEST_BODY);
        }
        if (length > MAX_BODY_BYTES) {
            return new Response(413, TOO_LARGE_BODY);
        }
        byte[] body = readBody(input, (int) length);
        if (body == null) {
            return null;
        }
        /* Reading a slow body can straddle token rotation. */
        if (!isAuthorized(headers.get("authorization"))) {
            return new Response(401, UNAUTHORIZED_BODY);
        }
        String remote = socket.getInetAddress() == null ? "" : socket.getInetAddress().getHostAddress();
        String userAgent = headers.get("user-agent");
        Request request = new Request(
                "POST",
                "/mcp",
                remote,
                userAgent == null ? "" : userAgent,
                new String(body, StandardCharsets.UTF_8));
        return forward(request);
    }

    private Response forward(Request request) {
        if (handler == null) {
            return new Response(503, NOT_READY_BODY);
        }
        CompletableFuture<Response> delivery = new CompletableFuture<>();
        inFlight.add(delivery);
        if (!running) {
            inFlight.remove(delivery);
            return new Response(503, NOT_READY_BODY);
        }
        CompletableFuture<Response> handled;
        try {
            handled = handler.handle(request);
        } catch (RuntimeException error) {
            inFlight.remove(delivery);
            return new Response(503, NOT_READY_BODY);
        }
        if (handled == null) {
            inFlight.remove(delivery);
            return new Response(503, NOT_READY_BODY);
        }
        handled.whenComplete((response, error) -> {
            if (error != null || response == null) {
                delivery.complete(new Response(503, NOT_READY_BODY));
            } else {
                delivery.complete(response);
            }
        });
        try {
            return delivery.get();
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            return new Response(503, NOT_READY_BODY);
        } catch (java.util.concurrent.ExecutionException failed) {
            return new Response(503, NOT_READY_BODY);
        } finally {
            inFlight.remove(delivery);
        }
    }

    private boolean isAuthorized(String authorization) {
        String expected = token;
        if (expected == null || expected.isEmpty() || authorization == null) {
            return false;
        }
        String prefix = "Bearer ";
        if (authorization.length() < prefix.length()
                || !authorization.regionMatches(true, 0, prefix, 0, prefix.length())) {
            return false;
        }
        byte[] provided = authorization.substring(prefix.length()).trim().getBytes(StandardCharsets.UTF_8);
        byte[] wanted = expected.getBytes(StandardCharsets.UTF_8);
        return MessageDigest.isEqual(provided, wanted);
    }

    private static byte[] readBody(InputStream input, int length) {
        if (length == 0) {
            return new byte[0];
        }
        byte[] body = new byte[length];
        int offset = 0;
        try {
            while (offset < length) {
                int read = input.read(body, offset, length - offset);
                if (read < 0) {
                    return null;
                }
                offset += read;
            }
        } catch (IOException error) {
            return null;
        }
        return body;
    }

    private static void writeResponse(Socket socket, Response response, String allow) throws IOException {
        int status = response == null ? 500 : response.status;
        if (status < 100 || status > 599) {
            status = 500;
        }
        String body = response == null || response.body == null ? "" : response.body;
        if (status == 202 || status == 204) {
            body = "";
        }
        byte[] bodyBytes = body.getBytes(StandardCharsets.UTF_8);
        StringBuilder head = new StringBuilder(160);
        head.append("HTTP/1.1 ").append(status).append(' ').append(reason(status)).append("\r\n");
        head.append("Content-Type: application/json; charset=utf-8\r\n");
        head.append("Cache-Control: no-store\r\n");
        head.append("Connection: close\r\n");
        if (allow != null) {
            head.append("Allow: ").append(allow).append("\r\n");
        }
        if (status != 204) {
            head.append("Content-Length: ").append(bodyBytes.length).append("\r\n");
        }
        head.append("\r\n");
        OutputStream output = socket.getOutputStream();
        output.write(head.toString().getBytes(StandardCharsets.US_ASCII));
        if (bodyBytes.length > 0) {
            output.write(bodyBytes);
        }
        output.flush();
    }

    private static String reason(int status) {
        switch (status) {
            case 200:
                return "OK";
            case 202:
                return "Accepted";
            case 204:
                return "No Content";
            case 400:
                return "Bad Request";
            case 401:
                return "Unauthorized";
            case 403:
                return "Forbidden";
            case 404:
                return "Not Found";
            case 405:
                return "Method Not Allowed";
            case 411:
                return "Length Required";
            case 413:
                return "Payload Too Large";
            case 500:
                return "Internal Server Error";
            case 503:
                return "Service Unavailable";
            case 504:
                return "Gateway Timeout";
            default:
                return "Status";
        }
    }

    private static void closeQuietly(Socket socket) {
        try {
            socket.close();
        } catch (IOException ignored) {
            // Already closed.
        }
    }

    /** Reads CRLF lines while enforcing a total byte budget. */
    private static final class LineReader {

        private final InputStream input;
        private final int limit;
        private int total;
        private boolean overflowed;

        LineReader(InputStream input, int limit) {
            this.input = input;
            this.limit = limit;
        }

        boolean overflowed() {
            return overflowed;
        }

        String readLine() throws IOException {
            ByteArrayOutputStream line = new ByteArrayOutputStream(128);
            int current;
            while ((current = input.read()) >= 0) {
                total++;
                if (total > limit) {
                    overflowed = true;
                    return null;
                }
                if (current == '\n') {
                    byte[] bytes = line.toByteArray();
                    int length = bytes.length;
                    if (length > 0 && bytes[length - 1] == '\r') {
                        length--;
                    }
                    return new String(bytes, 0, length, StandardCharsets.ISO_8859_1);
                }
                line.write(current);
            }
            return null;
        }
    }
}
