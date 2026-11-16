package dev.mcpack.agent;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.lang.reflect.InvocationTargetException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.concurrent.ExecutionException;

/**
 * Newline-delimited JSON-RPC over a loopback TCP socket.
 * Request:  {"id":1,"token":"...","method":"status","params":{}}
 * Response: {"id":1,"result":...} or {"id":1,"error":{"message":"..."}}
 */
final class Server implements Runnable {
    private final int port;
    private final String token;
    private final Handlers handlers;

    Server(int port, String token, Handlers handlers) {
        this.port = port;
        this.token = token;
        this.handlers = handlers;
    }

    @Override
    public void run() {
        ServerSocket ss = null;
        try {
            // Explicit IPv4: some loaders set java.net.preferIPv6Addresses, which makes the
            // "loopback address" ::1 while the launcher connects to 127.0.0.1.
            ss = new ServerSocket(port, 16, InetAddress.getByAddress(new byte[] {127, 0, 0, 1}));
            Log.info("listening on 127.0.0.1:" + port);
            while (!Thread.currentThread().isInterrupted()) {
                final Socket s = ss.accept();
                Thread t = new Thread(new Runnable() {
                    @Override
                    public void run() {
                        handle(s);
                    }
                }, "mcpack-agent-conn");
                t.setDaemon(true);
                t.start();
            }
        } catch (IOException e) {
            Log.error("server failed", e);
        } finally {
            if (ss != null) {
                try {
                    ss.close();
                } catch (IOException ignored) {
                }
            }
        }
    }

    @SuppressWarnings("unchecked")
    private void handle(Socket s) {
        try {
            BufferedReader in = new BufferedReader(new InputStreamReader(s.getInputStream(), "UTF-8"));
            Writer out = new OutputStreamWriter(s.getOutputStream(), "UTF-8");
            String line;
            while ((line = in.readLine()) != null) {
                if (line.trim().isEmpty()) continue;
                Map<String, Object> resp = new LinkedHashMap<String, Object>();
                Object id = null;
                try {
                    Map<String, Object> req = (Map<String, Object>) Json.parse(line);
                    id = req.get("id");
                    Object tok = req.get("token");
                    if (!token.equals(tok == null ? "" : tok.toString())) throw new AgentException("unauthorized");
                    String method = String.valueOf(req.get("method"));
                    Object params = req.get("params");
                    Map<String, Object> p = params instanceof Map ? (Map<String, Object>) params : new LinkedHashMap<String, Object>();
                    resp.put("result", handlers.dispatch(method, p));
                } catch (Throwable e) {
                    Throwable cause = e;
                    while ((cause instanceof ExecutionException || cause instanceof InvocationTargetException) && cause.getCause() != null) {
                        cause = cause.getCause();
                    }
                    if (!(cause instanceof AgentException)) Log.error("call failed", cause);
                    Map<String, Object> err = new LinkedHashMap<String, Object>();
                    err.put("message", cause.getMessage() == null ? cause.toString() : cause.getMessage());
                    err.put("type", cause.getClass().getName());
                    resp.put("error", err);
                }
                resp.put("id", id);
                out.write(Json.write(resp));
                out.write("\n");
                out.flush();
            }
        } catch (IOException ignored) {
            // client went away
        } finally {
            try {
                s.close();
            } catch (IOException ignored) {
            }
        }
    }
}
