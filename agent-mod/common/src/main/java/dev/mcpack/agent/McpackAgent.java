package dev.mcpack.agent;

import net.fabricmc.api.ClientModInitializer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * Entry point. The agent does nothing unless the launcher passes
 * {@code -Dmcpack.agent.port} (and {@code -Dmcpack.agent.token}) on the JVM command line.
 */
public final class McpackAgent implements ClientModInitializer {
    public static final Logger LOG = LoggerFactory.getLogger("mcpack-agent");

    @Override
    public void onInitializeClient() {
        String port = System.getProperty("mcpack.agent.port");
        if (port == null || port.isBlank()) {
            LOG.info("[mcpack-agent] no mcpack.agent.port set; agent disabled");
            return;
        }
        String token = System.getProperty("mcpack.agent.token", "");
        VersionOps ops;
        try {
            ops = (VersionOps) Class.forName("dev.mcpack.agent.VersionOpsImpl").getDeclaredConstructor().newInstance();
        } catch (ReflectiveOperationException e) {
            LOG.error("[mcpack-agent] failed to load version ops", e);
            return;
        }
        Thread t = new Thread(new AgentServer(Integer.parseInt(port.trim()), token, new Handlers(ops)), "mcpack-agent");
        t.setDaemon(true);
        t.start();
    }
}
