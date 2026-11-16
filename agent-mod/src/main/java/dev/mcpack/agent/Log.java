package dev.mcpack.agent;

/** stdout logging (the launcher captures the game's output). */
final class Log {
    private Log() {}

    static void info(String msg) {
        System.out.println("[mcpack-agent] " + msg);
    }

    static void error(String msg, Throwable e) {
        System.out.println("[mcpack-agent] ERROR " + msg + (e == null ? "" : ": " + e));
        if (e != null) {
            e.printStackTrace(System.out);
        }
    }
}
