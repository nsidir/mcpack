package dev.mcpack.agent;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonPrimitive;
import com.mojang.blaze3d.platform.NativeImage;
import net.fabricmc.loader.api.FabricLoader;
import net.minecraft.SharedConstants;
import net.minecraft.client.Minecraft;
import net.minecraft.client.Screenshot;
import net.minecraft.client.gui.screens.TitleScreen;
import net.minecraft.client.server.IntegratedServer;
import net.minecraft.commands.CommandSource;
import net.minecraft.commands.CommandSourceStack;
import net.minecraft.network.chat.Component;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.server.packs.repository.PackRepository;
import net.minecraft.world.level.storage.LevelResource;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.function.Supplier;

final class Handlers {
    private final VersionOps ops;

    Handlers(VersionOps ops) {
        this.ops = ops;
    }

    JsonElement dispatch(String method, JsonObject p) throws Exception {
        switch (method) {
            case "ping":
                return new JsonPrimitive("pong");
            case "status":
                return onClient(this::status);
            case "command":
                return command(str(p, "command", null));
            case "client_command":
                return onClient(() -> {
                    Minecraft mc = Minecraft.getInstance();
                    if (mc.player == null) throw new AgentException("not in a world");
                    String c = stripSlash(str(p, "command", null));
                    mc.player.connection.sendCommand(c);
                    return ok();
                });
            case "chat":
                return onClient(() -> {
                    Minecraft mc = Minecraft.getInstance();
                    if (mc.player == null) throw new AgentException("not in a world");
                    mc.player.connection.sendChat(str(p, "message", null));
                    return ok();
                });
            case "reload_resources":
                return reloadResources(p);
            case "reload_datapacks":
                return command("reload");
            case "screenshot":
                return screenshot(p);
            case "create_world": {
                String levelId = str(p, "level_id", "mcpack-test");
                String name = str(p, "name", levelId);
                Long seed = p.has("seed") && !p.get("seed").isJsonNull() ? p.get("seed").getAsLong() : null;
                String mode = str(p, "game_mode", "creative");
                boolean flat = bool(p, "flat", false);
                boolean cheats = bool(p, "cheats", true);
                Minecraft.getInstance().execute(() -> ops.createWorld(levelId, name, seed, mode, flat, cheats));
                return ok();
            }
            case "join_world": {
                String levelId = str(p, "level_id", null);
                Minecraft.getInstance().execute(() -> ops.joinWorld(levelId));
                return ok();
            }
            case "leave_world":
                Minecraft.getInstance().execute(ops::leaveWorld);
                return ok();
            case "exit":
                Minecraft.getInstance().execute(() -> Minecraft.getInstance().stop());
                return ok();
            default:
                throw new AgentException("unknown method: " + method);
        }
    }

    private JsonObject status() {
        Minecraft mc = Minecraft.getInstance();
        JsonObject o = new JsonObject();
        o.addProperty("minecraft", SharedConstants.getCurrentVersion().getName());
        o.addProperty("mods_loaded", FabricLoader.getInstance().getAllMods().size());
        o.addProperty("screen", mc.screen == null ? null : mc.screen.getClass().getName());
        o.addProperty("screen_title", mc.screen == null ? null : mc.screen.getTitle().getString());
        o.addProperty("title_screen", mc.screen instanceof TitleScreen);
        o.addProperty("loading", mc.getOverlay() != null);
        o.addProperty("in_world", mc.level != null && mc.player != null);
        o.addProperty("singleplayer", mc.hasSingleplayerServer());
        o.addProperty("fps", mc.getFps());
        IntegratedServer server = mc.getSingleplayerServer();
        if (server != null) {
            o.addProperty("world_name", server.getWorldData().getLevelName());
            Path root = server.getWorldPath(LevelResource.ROOT).toAbsolutePath().normalize();
            o.addProperty("world_dir", root.toString());
            o.addProperty("level_id", root.getFileName() == null ? null : root.getFileName().toString());
        }
        if (mc.player != null && mc.level != null) {
            JsonObject pos = new JsonObject();
            pos.addProperty("x", mc.player.getX());
            pos.addProperty("y", mc.player.getY());
            pos.addProperty("z", mc.player.getZ());
            o.add("position", pos);
            o.addProperty("dimension", mc.level.dimension().location().toString());
            o.addProperty("health", mc.player.getHealth());
        }
        return o;
    }

    private JsonElement command(String raw) throws Exception {
        String cmd = stripSlash(raw);
        Minecraft mc = Minecraft.getInstance();
        IntegratedServer server = mc.getSingleplayerServer();
        if (server == null) throw new AgentException("command requires a singleplayer world (use create_world/join_world first)");
        List<String> output = Collections.synchronizedList(new ArrayList<>());
        CommandSource capture = new CommandSource() {
            @Override public void sendSystemMessage(Component message) { output.add(message.getString()); }
            @Override public boolean acceptsSuccess() { return true; }
            @Override public boolean acceptsFailure() { return true; }
            @Override public boolean shouldInformAdmins() { return false; }
        };
        CompletableFuture<Void> fut = server.submit(() -> {
            CommandSourceStack stack;
            ServerPlayer player = mc.player == null ? null : server.getPlayerList().getPlayer(mc.player.getUUID());
            stack = player != null ? player.createCommandSourceStack() : server.createCommandSourceStack();
            stack = stack.withSource(capture).withPermission(4);
            server.getCommands().performPrefixedCommand(stack, cmd);
            return null;
        });
        fut.get(60, TimeUnit.SECONDS);
        JsonObject o = new JsonObject();
        JsonArray lines = new JsonArray();
        synchronized (output) {
            for (String s : output) lines.add(s);
        }
        o.add("output", lines);
        return o;
    }

    private JsonElement reloadResources(JsonObject p) throws Exception {
        Set<String> enable = new LinkedHashSet<>();
        if (p.has("enable") && p.get("enable").isJsonArray()) {
            for (JsonElement e : p.getAsJsonArray("enable")) enable.add(e.getAsString());
        }
        CompletableFuture<Void> reload = onClient(() -> {
            Minecraft mc = Minecraft.getInstance();
            PackRepository repo = mc.getResourcePackRepository();
            repo.reload();
            if (!enable.isEmpty()) {
                Set<String> selected = new LinkedHashSet<>(repo.getSelectedIds());
                for (String id : enable) {
                    if (repo.getAvailableIds().contains(id)) selected.add(id);
                }
                repo.setSelected(selected);
                mc.options.updateResourcePacks(repo);
                return CompletableFuture.<Void>completedFuture(null);
            }
            return mc.reloadResourcePacks();
        });
        reload.get(180, TimeUnit.SECONDS);
        JsonObject o = ok();
        JsonArray selected = new JsonArray();
        onClient(() -> {
            for (String id : Minecraft.getInstance().getResourcePackRepository().getSelectedIds()) selected.add(id);
            return null;
        });
        o.add("selected", selected);
        return o;
    }

    private JsonElement screenshot(JsonObject p) throws Exception {
        NativeImage image = onClient(() -> Screenshot.takeScreenshot(Minecraft.getInstance().getMainRenderTarget()));
        Path dir = Minecraft.getInstance().gameDirectory.toPath().resolve("screenshots");
        Files.createDirectories(dir);
        Path file = dir.resolve(str(p, "name", "mcpack-" + System.currentTimeMillis()) + ".png");
        try {
            image.writeToFile(file);
        } finally {
            image.close();
        }
        JsonObject o = ok();
        o.addProperty("path", file.toAbsolutePath().toString());
        return o;
    }

    // ---- helpers ----

    static <T> T onClient(Supplier<T> s) throws Exception {
        return Minecraft.getInstance().submit(s).get(60, TimeUnit.SECONDS);
    }

    private static JsonObject ok() {
        JsonObject o = new JsonObject();
        o.addProperty("ok", true);
        return o;
    }

    private static String stripSlash(String c) {
        if (c == null) throw new AgentException("missing 'command'");
        c = c.trim();
        return c.startsWith("/") ? c.substring(1) : c;
    }

    private static String str(JsonObject p, String key, String def) {
        if (p.has(key) && !p.get(key).isJsonNull()) return p.get(key).getAsString();
        if (def == null) throw new AgentException("missing '" + key + "'");
        return def;
    }

    private static boolean bool(JsonObject p, String key, boolean def) {
        return p.has(key) && !p.get(key).isJsonNull() ? p.get(key).getAsBoolean() : def;
    }
}
