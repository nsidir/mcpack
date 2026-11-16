package dev.mcpack.agent;

import java.lang.instrument.Instrumentation;

/**
 * Java agent entry point ({@code -javaagent:mcpack-agent.jar}). Works the same on every mod
 * loader (and vanilla) because it is not a mod: it waits for the game to start, then drives it
 * through reflection, translating Mojang names to the runtime names with a mappings file the
 * launcher writes ({@code -Dmcpack.agent.mappings}).
 *
 * <p>Inactive unless the launcher passes {@code -Dmcpack.agent.port}.
 */
public final class Boot {
    private Boot() {}

    public static void premain(String args, Instrumentation inst) {
        start(inst);
    }

    public static void agentmain(String args, Instrumentation inst) {
        start(inst);
    }

    private static void start(final Instrumentation inst) {
        final String port = System.getProperty("mcpack.agent.port");
        if (port == null || port.trim().isEmpty()) {
            return;
        }
        final String token = System.getProperty("mcpack.agent.token", "");
        final String mappings = System.getProperty("mcpack.agent.mappings");
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                try {
                    Game game = new Game(inst, Mappings.load(mappings));
                    new Server(Integer.parseInt(port.trim()), token, new Handlers(game)).run();
                } catch (Throwable e) {
                    Log.error("agent failed to start", e);
                }
            }
        }, "mcpack-agent");
        t.setDaemon(true);
        t.start();
    }
}
