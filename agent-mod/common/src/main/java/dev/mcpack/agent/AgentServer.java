package dev.mcpack.agent;

import com.google.gson.JsonElement;
import com.google.gson.JsonNull;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.ExecutionException;

/**
 * Newline-delimited JSON-RPC over a loopback TCP socket.
 * Request:  {"id":1,"token":"...","method":"status","params":{}}
 * Response: {"id":1,"result":...} or {"id":1,"error":{"message":"..."}}
 */
final class AgentServer implements Runnable {
    private final int port;
    private final String token;
    private final Handlers handlers;

    AgentServer(int port, String token, Handlers handlers) {
        this.port = port;
        this.token = token;
        this.handlers = handlers;
    }

    @Override
    public void run() {
        try (ServerSocket ss = new ServerSocket(port, 16, InetAddress.getLoopbackAddress())) {
            McpackAgent.LOG.info("[mcpack-agent] listening on 127.0.0.1:{}", port);
            while (!Thread.currentThread().isInterrupted()) {
                Socket s = ss.accept();
                Thread t = new Thread(() -> handle(s), "mcpack-agent-conn");
                t.setDaemon(true);
                t.start();
            }
        } catch (IOException e) {
            McpackAgent.LOG.error("[mcpack-agent] server failed", e);
        }
    }

    private void handle(Socket s) {
        try (s;
             BufferedReader in = new BufferedReader(new InputStreamReader(s.getInputStream(), StandardCharsets.UTF_8));
             Writer out = new OutputStreamWriter(s.getOutputStream(), StandardCharsets.UTF_8)) {
            String line;
            while ((line = in.readLine()) != null) {
                if (line.isBlank()) continue;
                JsonObject resp = new JsonObject();
                JsonElement id = JsonNull.INSTANCE;
                try {
                    JsonObject req = JsonParser.parseString(line).getAsJsonObject();
                    if (req.has("id")) id = req.get("id");
                    String tok = req.has("token") ? req.get("token").getAsString() : "";
                    if (!token.equals(tok)) throw new AgentException("unauthorized");
                    String method = req.get("method").getAsString();
                    JsonObject params = req.has("params") && req.get("params").isJsonObject()
                            ? req.getAsJsonObject("params") : new JsonObject();
                    resp.add("result", handlers.dispatch(method, params));
                } catch (Throwable e) {
                    Throwable cause = e;
                    while (cause instanceof ExecutionException && cause.getCause() != null) cause = cause.getCause();
                    JsonObject err = new JsonObject();
                    err.addProperty("message", cause.getMessage() == null ? cause.toString() : cause.getMessage());
                    err.addProperty("type", cause.getClass().getName());
                    resp.add("error", err);
                }
                resp.add("id", id);
                out.write(resp.toString());
                out.write("\n");
                out.flush();
            }
        } catch (IOException ignored) {
            // client went away
        }
    }
}
