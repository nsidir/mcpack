package dev.mcpack.agent;

import java.io.File;
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.util.ArrayList;
import java.util.Collection;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.Callable;
import java.util.concurrent.Executor;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import java.util.function.Consumer;

/**
 * The agent's methods for Minecraft 1.14.4 and newer (Mojang names). Each operation feature-
 * detects the API of the running version instead of switching on version numbers.
 * Pre-1.13 Forge (MCP names) is handled by {@link Legacy}.
 */
final class Handlers {
    private final Game g;
    private final Legacy legacy;

    Handlers(Game g) {
        this.g = g;
        this.legacy = g.legacy() ? new Legacy(g) : null;
    }

    Object dispatch(String method, final Map<String, Object> p) throws Exception {
        if (method.equals("ping")) return "pong";
        if (legacy != null) return legacy.dispatch(method, p);
        switch (method) {
            case "status":
                return status();
            case "command":
                return command(str(p, "command", null));
            case "client_command":
                return clientCommand(stripSlash(str(p, "command", null)));
            case "chat":
                return chat(str(p, "message", null));
            case "reload_resources":
                return reloadResources(p);
            case "reload_datapacks":
                return command("reload");
            case "screenshot":
                return screenshot(p);
            case "join_world":
                return joinWorld(str(p, "level_id", null));
            case "leave_world":
                return leaveWorld();
            case "exit":
                g.onClient(new Callable<Object>() {
                    @Override
                    public Object call() {
                        return g.call(g.requireMc(), "stop");
                    }
                });
                return ok();
            default:
                throw new AgentException("unknown method: " + method);
        }
    }

    // ------------------------------------------------------------------ status

    private Object screen(Object mc) {
        if (g.hasField(mc, "screen")) return g.get(mc, "screen");
        Object gui = g.getOrNull(mc, "gui");
        return gui != null && g.has(gui, "screen", 0) ? g.call(gui, "screen") : null;
    }

    private void setScreen(Object mc, Object screen) {
        Object gui = g.getOrNull(mc, "gui");
        if (!g.hasField(mc, "screen") && gui != null && g.has(gui, "setScreen", 1)) {
            g.call(gui, "setScreen", screen);
        } else {
            g.call(mc, "setScreen", screen);
        }
    }

    private Object overlay(Object mc) {
        if (g.has(mc, "getOverlay", 0)) return g.call(mc, "getOverlay");
        Object gui = g.getOrNull(mc, "gui");
        return gui != null && g.has(gui, "overlay", 0) ? g.call(gui, "overlay") : null;
    }

    private String text(Object component) {
        if (component == null) return null;
        try {
            return String.valueOf(g.call(component, "getString"));
        } catch (RuntimeException e) {
            return component.toString();
        }
    }

    static String loaderName() {
        String l = System.getProperty("mcpack.agent.loader");
        return l == null ? "unknown" : l;
    }

    /** Number of loaded mods, whatever the loader. */
    private Integer modCount() {
        ClassLoader cl = g.requireMc().getClass().getClassLoader();
        String[][] probes = {
            {"net.fabricmc.loader.api.FabricLoader", "getInstance", "getAllMods"},
            {"net.neoforged.fml.ModList", "get", "getMods"},
            {"net.minecraftforge.fml.ModList", "get", "getMods"},
        };
        for (String[] p : probes) {
            try {
                Class<?> c = Class.forName(p[0], true, cl);
                Object inst = c.getMethod(p[1]).invoke(null);
                if (inst == null) continue;
                Object mods = inst.getClass().getMethod(p[2]).invoke(inst);
                if (mods instanceof Collection) return ((Collection<?>) mods).size();
            } catch (Throwable ignored) {
            }
        }
        return null;
    }

    private Map<String, Object> status() throws Exception {
        if (g.mc() == null) {
            Map<String, Object> o = new LinkedHashMap<String, Object>();
            o.put("minecraft", System.getProperty("mcpack.agent.mc"));
            o.put("loader", loaderName());
            o.put("loading", true);
            o.put("title_screen", false);
            o.put("in_world", false);
            o.put("starting", true);
            return o;
        }
        return g.onClient(new Callable<Map<String, Object>>() {
            @Override
            public Map<String, Object> call() {
                Object mc = g.requireMc();
                Map<String, Object> o = new LinkedHashMap<String, Object>();
                o.put("minecraft", System.getProperty("mcpack.agent.mc"));
                o.put("loader", loaderName());
                o.put("mods_loaded", modCount());
                Object screen = screen(mc);
                o.put("screen", screen == null ? null : g.namedClassOf(screen.getClass()));
                Object title = null;
                if (screen != null) {
                    try {
                        title = text(g.call(screen, "getTitle"));
                    } catch (RuntimeException ignored) {
                    }
                }
                o.put("screen_title", title);
                o.put("title_screen", g.isInstance("net.minecraft.client.gui.screens.TitleScreen", screen));
                String screenName = screen == null ? "" : g.namedClassOf(screen.getClass());
                o.put("loading", overlay(mc) != null || isLoadingScreen(screenName));
                Object level = g.get(mc, "level");
                Object player = g.get(mc, "player");
                o.put("in_world", level != null && player != null);
                o.put("singleplayer", g.call(mc, "hasSingleplayerServer"));
                Object fps = g.has(mc, "getFps", 0) ? g.call(mc, "getFps") : g.getOrNull(mc.getClass(), "fps");
                o.put("fps", fps);
                Object server = g.call(mc, "getSingleplayerServer");
                if (server != null) {
                    try {
                        if (g.has(server, "getWorldPath", 1)) {
                            Object root = g.get(g.cls("net.minecraft.world.level.storage.LevelResource"), "ROOT");
                            java.nio.file.Path dir = ((java.nio.file.Path) g.call(server, "getWorldPath", root)).toAbsolutePath().normalize();
                            o.put("world_dir", dir.toString());
                            o.put("level_id", dir.getFileName() == null ? null : dir.getFileName().toString());
                            o.put("world_name", g.call(g.call(server, "getWorldData"), "getLevelName"));
                        } else {
                            String id = String.valueOf(g.call(server, "getLevelIdName"));
                            o.put("level_id", id);
                            o.put("world_dir", new File((File) g.get(mc, "gameDirectory"), "saves/" + id).getAbsolutePath());
                            o.put("world_name", g.call(server, "getLevelName"));
                        }
                    } catch (RuntimeException e) {
                        o.put("world_error", e.toString());
                    }
                }
                if (player != null && level != null) {
                    Map<String, Object> pos = new LinkedHashMap<String, Object>();
                    for (String axis : new String[] {"x", "y", "z"}) {
                        String getter = "get" + axis.toUpperCase();
                        pos.put(axis, g.has(player, getter, 0) ? g.call(player, getter) : g.get(player, axis));
                    }
                    o.put("position", pos);
                    try {
                        if (g.has(level, "dimension", 0)) {
                            String key = String.valueOf(g.call(level, "dimension"));
                            // ResourceKey[minecraft:dimension / minecraft:overworld]
                            int slash = key.lastIndexOf("/ ");
                            o.put("dimension", slash >= 0 ? key.substring(slash + 2, key.length() - 1) : key);
                        }
                    } catch (RuntimeException ignored) {
                    }
                    o.put("health", g.call(player, "getHealth"));
                }
                return o;
            }
        });
    }

    /** Screens shown while a world loads or saves (the world is not playable yet). */
    private static boolean isLoadingScreen(String named) {
        String simple = named.substring(named.lastIndexOf('.') + 1);
        return simple.equals("ReceivingLevelScreen") || simple.equals("LevelLoadingScreen") || simple.equals("ProgressScreen")
                || simple.equals("GenericDirtMessageScreen") || simple.equals("GenericMessageScreen") || simple.equals("ConnectScreen")
                || simple.equals("GenericWaitingScreen") || simple.equals("WorldLoadingScreen");
    }

    // ------------------------------------------------------------------ commands

    /** A CommandSource that records every message sent to it. */
    private Object captureSource(final List<String> output) {
        final Class<?> iface = g.cls("net.minecraft.commands.CommandSource");
        final Class<?> component = g.cls("net.minecraft.network.chat.Component");
        final String owner = "net.minecraft.commands.CommandSource";
        return Proxy.newProxyInstance(iface.getClassLoader(), new Class<?>[] {iface}, new InvocationHandler() {
            @Override
            public Object invoke(Object proxy, Method m, Object[] args) {
                String name = m.getName();
                if (m.getDeclaringClass() == Object.class) {
                    if (name.equals("equals")) return proxy == args[0];
                    if (name.equals("hashCode")) return System.identityHashCode(proxy);
                    return "mcpack-agent";
                }
                if (args != null) {
                    for (Object a : args) {
                        if (component.isInstance(a)) {
                            output.add(text(a));
                            return null;
                        }
                    }
                }
                String named = g.mappings().namedMethod(owner, name, m.getParameterTypes().length);
                Class<?> r = m.getReturnType();
                if (r == boolean.class) return named.equals("acceptsSuccess") || named.equals("acceptsFailure");
                if (r == int.class) return 0;
                if (r == long.class) return 0L;
                if (r == float.class) return 0f;
                if (r == double.class) return 0d;
                return null;
            }
        });
    }

    private Object elevate(Object stack, Object server) {
        if (g.find(stack.getClass(), "withPermission", new Object[] {4}) != null) {
            return g.call(stack, "withPermission", 4);
        }
        Class<?> perms = g.clsOrNull("net.minecraft.server.permissions.PermissionSet");
        if (perms != null) {
            Object all = g.getOrNull(perms, "ALL_PERMISSIONS");
            if (all != null) return g.call(stack, "withPermission", all);
        }
        return stack;
    }

    private Map<String, Object> command(String raw) throws Exception {
        final String cmd = stripSlash(raw);
        final Object mc = g.requireMc();
        final Object server = g.call(mc, "getSingleplayerServer");
        if (server == null) throw new AgentException("command requires a singleplayer world (use world_enter first)");
        final List<String> output = Collections.synchronizedList(new ArrayList<String>());
        g.onThread((Executor) server, new Callable<Object>() {
            @Override
            public Object call() {
                Object player = g.get(mc, "player");
                Object sp = null;
                if (player != null) {
                    UUID id = (UUID) g.call(player, "getUUID");
                    sp = g.call(g.call(server, "getPlayerList"), "getPlayer", id);
                }
                Object stack = sp != null ? g.call(sp, "createCommandSourceStack") : g.call(server, "createCommandSourceStack");
                Object source = captureSource(output);
                if (g.has(stack, "withSource", 1)) {
                    stack = g.call(stack, "withSource", source);
                } else {
                    // 1.14 – 1.18: no withSource; the stack is a fresh copy, so swap its source.
                    g.set(stack, "source", source);
                }
                stack = elevate(stack, server);
                Object commands = g.call(server, "getCommands");
                if (g.has(commands, "performPrefixedCommand", 2)) {
                    g.call(commands, "performPrefixedCommand", stack, cmd);
                } else {
                    g.call(commands, "performCommand", stack, "/" + cmd);
                }
                return null;
            }
        }, 60);
        Map<String, Object> o = new LinkedHashMap<String, Object>();
        synchronized (output) {
            o.put("output", new ArrayList<String>(output));
        }
        return o;
    }

    private Map<String, Object> clientCommand(final String cmd) throws Exception {
        g.onClient(new Callable<Object>() {
            @Override
            public Object call() {
                Object player = g.get(g.requireMc(), "player");
                if (player == null) throw new AgentException("not in a world");
                Object conn = g.getOrNull(player, "connection");
                if (conn != null && g.has(conn, "sendCommand", 1)) return g.call(conn, "sendCommand", cmd);
                if (g.has(player, "commandSigned", 2)) return g.call(player, "commandSigned", cmd, null);
                if (g.has(player, "command", 1)) return g.call(player, "command", cmd);
                return g.call(player, "chat", "/" + cmd);
            }
        });
        return ok();
    }

    private Map<String, Object> chat(final String message) throws Exception {
        g.onClient(new Callable<Object>() {
            @Override
            public Object call() {
                Object player = g.get(g.requireMc(), "player");
                if (player == null) throw new AgentException("not in a world");
                Object conn = g.getOrNull(player, "connection");
                if (conn != null && g.has(conn, "sendChat", 1)) return g.call(conn, "sendChat", message);
                if (g.has(player, "chatSigned", 2)) return g.call(player, "chatSigned", message, null);
                return g.call(player, "chat", message);
            }
        });
        return ok();
    }

    // ------------------------------------------------------------------ resources

    @SuppressWarnings("unchecked")
    private Map<String, Object> reloadResources(Map<String, Object> p) throws Exception {
        final Set<String> enable = new LinkedHashSet<String>();
        Object e = p.get("enable");
        if (e instanceof List) {
            for (Object x : (List<Object>) e) enable.add(String.valueOf(x));
        }
        final Object mc = g.requireMc();
        Object pending = g.onClient(new Callable<Object>() {
            @Override
            public Object call() {
                Object repo = g.call(mc, "getResourcePackRepository");
                g.call(repo, "reload");
                if (enable.isEmpty()) return g.call(mc, "reloadResourcePacks");
                Object options = g.get(mc, "options");
                if (g.has(repo, "getSelectedIds", 0)) {
                    Set<String> selected = new LinkedHashSet<String>((Collection<String>) g.call(repo, "getSelectedIds"));
                    Collection<String> available = (Collection<String>) g.call(repo, "getAvailableIds");
                    for (String id : enable) if (available.contains(id)) selected.add(id);
                    g.call(repo, "setSelected", selected);
                    if (g.has(options, "updateResourcePacks", 1)) {
                        g.call(options, "updateResourcePacks", repo);
                        return null; // reloads by itself when the selection changed
                    }
                    List<String> packs = (List<String>) g.get(options, "resourcePacks");
                    for (String id : selected) if (!packs.contains(id) && !id.equals("vanilla")) packs.add(id);
                } else {
                    List<String> packs = (List<String>) g.get(options, "resourcePacks");
                    for (String id : enable) if (!packs.contains(id)) packs.add(id);
                    for (Method m : options.getClass().getDeclaredMethods()) {
                        Class<?>[] ps = m.getParameterTypes();
                        if (ps.length == 1 && ps[0].isInstance(repo) && m.getReturnType() == void.class) {
                            g.invoke(m, options, repo);
                            break;
                        }
                    }
                }
                g.call(options, "save");
                return g.call(mc, "reloadResourcePacks");
            }
        });
        if (pending instanceof Future) {
            ((Future<?>) pending).get(180, TimeUnit.SECONDS);
        } else {
            waitForOverlay(mc, 180);
        }
        Map<String, Object> o = ok();
        o.put("selected", g.onClient(new Callable<Object>() {
            @Override
            public Object call() {
                Object repo = g.call(mc, "getResourcePackRepository");
                if (g.has(repo, "getSelectedIds", 0)) return new ArrayList<Object>((Collection<Object>) g.call(repo, "getSelectedIds"));
                return new ArrayList<Object>((List<Object>) g.get(g.get(mc, "options"), "resourcePacks"));
            }
        }));
        return o;
    }

    private void waitForOverlay(final Object mc, int secs) throws Exception {
        long end = System.currentTimeMillis() + secs * 1000L;
        Thread.sleep(300);
        while (System.currentTimeMillis() < end) {
            Object ov = g.onClient(new Callable<Object>() {
                @Override
                public Object call() {
                    return overlay(mc);
                }
            });
            if (ov == null) return;
            Thread.sleep(200);
        }
        throw new AgentException("resource reload did not finish");
    }

    // ------------------------------------------------------------------ screenshots

    private Map<String, Object> screenshot(Map<String, Object> p) throws Exception {
        final Object mc = g.requireMc();
        final String name = str(p, "name", "mcpack-" + System.currentTimeMillis());
        final File dir = new File((File) g.get(mc, "gameDirectory"), "screenshots");
        final File target = new File(dir, name + ".png");
        final long started = System.currentTimeMillis();
        g.onClient(new Callable<Object>() {
            @Override
            public Object call() {
                Class<?> shots = g.cls("net.minecraft.client.Screenshot");
                Object renderTarget = g.has(mc, "getMainRenderTarget", 0) ? g.call(mc, "getMainRenderTarget") : null;
                Consumer<Object> done = new Consumer<Object>() {
                    @Override
                    public void accept(Object msg) {
                    }
                };
                for (Method m : g.methods(shots, "grab")) {
                    Class<?>[] ps = m.getParameterTypes();
                    if (!containsType(ps, String.class) || (renderTarget == null && containsRenderTarget(ps))) continue;
                    int ints = 0;
                    for (Class<?> c : ps) if (c == int.class) ints++;
                    Object[] args = new Object[ps.length];
                    int intIndex = 0;
                    for (int i = 0; i < ps.length; i++) {
                        Class<?> c = ps[i];
                        if (c == File.class) args[i] = g.get(mc, "gameDirectory");
                        else if (c == String.class) args[i] = target.getName();
                        else if (c == int.class) {
                            // (width, height) before 1.17; a downscale factor since 1.21.6.
                            args[i] = ints == 2 ? g.get(renderTarget, intIndex++ == 0 ? "width" : "height") : 1;
                        } else if (Consumer.class.isAssignableFrom(c)) args[i] = done;
                        else if (renderTarget != null && c.isInstance(renderTarget)) args[i] = renderTarget;
                        else args[i] = null;
                    }
                    g.invoke(m, null, args);
                    return null;
                }
                // No named variant (26.x+): the F2 path, then rename the newest file.
                for (Method m : g.methods(shots, "grab")) {
                    Class<?>[] ps = m.getParameterTypes();
                    if (ps.length == 2 && ps[0].isInstance(mc) && ps[1] == boolean.class) {
                        g.invoke(m, null, mc, false);
                        return null;
                    }
                }
                throw new AgentException("no usable Screenshot.grab in this version");
            }
        });
        // Screenshots are written asynchronously: wait for the file.
        long end = System.currentTimeMillis() + 20000;
        File found = null;
        while (System.currentTimeMillis() < end) {
            if (target.isFile() && target.length() > 0) {
                found = target;
            } else {
                File[] files = dir.listFiles();
                if (files != null) {
                    for (File f : files) {
                        if (f.getName().endsWith(".png") && f.lastModified() >= started - 1000 && f.length() > 0) found = f;
                    }
                }
            }
            if (found != null) {
                long len = found.length();
                Thread.sleep(250);
                if (found.length() == len) break;
            } else {
                Thread.sleep(100);
            }
        }
        if (found == null) throw new AgentException("screenshot was not written");
        if (!found.equals(target) && found.renameTo(target)) found = target;
        Map<String, Object> o = ok();
        o.put("path", found.getAbsolutePath());
        return o;
    }

    private static boolean containsType(Class<?>[] ps, Class<?> t) {
        for (Class<?> c : ps) if (c == t) return true;
        return false;
    }

    private boolean containsRenderTarget(Class<?>[] ps) {
        Class<?> rt = g.clsOrNull("com.mojang.blaze3d.pipeline.RenderTarget");
        if (rt == null) return false;
        for (Class<?> c : ps) if (c.isAssignableFrom(rt)) return true;
        return false;
    }

    // ------------------------------------------------------------------ worlds

    private Map<String, Object> joinWorld(final String levelId) throws Exception {
        final Object mc = g.requireMc();
        // Scheduled (not awaited): loading a world blocks the client thread until it is in.
        ((Executor) mc).execute(new Runnable() {
            @Override
            public void run() {
                try {
                    Runnable back = new Runnable() {
                        @Override
                        public void run() {
                            setScreen(mc, null);
                        }
                    };
                    if (g.has(mc, "createWorldOpenFlows", 0)) {
                        Object flows = g.call(mc, "createWorldOpenFlows");
                        if (g.find(flows.getClass(), "openWorld", new Object[] {levelId, back}) != null) {
                            g.call(flows, "openWorld", levelId, back);
                        } else if (g.find(flows.getClass(), "checkForBackupAndLoad", new Object[] {levelId, back}) != null) {
                            g.call(flows, "checkForBackupAndLoad", levelId, back);
                        } else {
                            Object screen = screen(mc);
                            if (screen == null) screen = g.construct(g.cls("net.minecraft.client.gui.screens.TitleScreen"));
                            g.call(flows, "loadLevel", screen, levelId);
                        }
                    } else if (g.has(mc, "loadLevel", 1)) {
                        g.call(mc, "loadLevel", levelId);
                    } else {
                        g.call(mc, "selectLevel", levelId, levelId, null);
                    }
                } catch (Throwable t) {
                    Log.error("join_world " + levelId + " failed", t);
                }
            }
        });
        confirmUpgradePrompt(mc);
        return ok();
    }

    /**
     * Newer versions (26.x) ask before upgrading a world saved by an older version — mcpack writes
     * new worlds in an old format — first "create a backup?" (BackupConfirmScreen), then "upgrade
     * completed, join the world?" (a ConfirmScreen). While a join is in progress, answer them:
     * load without a backup, then join.
     */
    private void confirmUpgradePrompt(final Object mc) {
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                long end = System.currentTimeMillis() + 180000;
                try {
                    while (System.currentTimeMillis() < end) {
                        Thread.sleep(400);
                        Boolean inWorld = g.onClient(new Callable<Boolean>() {
                            @Override
                            public Boolean call() throws Exception {
                                if (g.get(mc, "level") != null && g.get(mc, "player") != null) return true;
                                answerPrompt(screen(mc));
                                return false;
                            }
                        });
                        if (Boolean.TRUE.equals(inWorld)) return;
                    }
                } catch (Throwable e) {
                    Log.error("answering world prompts", e);
                }
            }
        }, "mcpack-agent-join");
        t.setDaemon(true);
        t.start();
    }

    /** Answer a world-upgrade prompt if {@code screen} is one. Runs on the client thread. */
    private void answerPrompt(Object screen) throws Exception {
        if (screen == null) return;
        String name = g.namedClassOf(screen.getClass());
        boolean backup = name.endsWith("BackupConfirmScreen");
        boolean confirm = name.endsWith(".ConfirmScreen");
        if (!backup && !confirm) return;
        if (confirm) {
            // Only the upgrade/join confirmation, not arbitrary questions.
            String title = "";
            try {
                title = text(g.call(screen, "getTitle")).toLowerCase(java.util.Locale.ROOT);
            } catch (RuntimeException ignored) {
            }
            if (!(title.contains("upgrad") || title.contains("world"))) return;
        }
        for (Class<?> k = screen.getClass(); k != null && k != Object.class; k = k.getSuperclass()) {
            for (java.lang.reflect.Field f : k.getDeclaredFields()) {
                Class<?> type = f.getType();
                if (!type.isInterface() || java.lang.reflect.Modifier.isStatic(f.getModifiers())) continue;
                f.setAccessible(true);
                Object cb = f.get(screen);
                if (cb == null) continue;
                // BackupConfirmScreen.Listener.proceed(backup=false, eraseCache=false);
                // ConfirmScreen's BooleanConsumer.accept(true).
                for (Method m : type.getMethods()) {
                    if (!java.lang.reflect.Modifier.isAbstract(m.getModifiers())) continue;
                    Class<?>[] ps = m.getParameterTypes();
                    if (ps.length == 0 || ps[0] != boolean.class) continue;
                    Object[] args = new Object[ps.length];
                    for (int i = 0; i < ps.length; i++) args[i] = ps[i] == boolean.class ? (Boolean) (confirm && i == 0) : null;
                    Log.info("answering " + name.substring(name.lastIndexOf('.') + 1) + " to join the world");
                    g.invoke(m, cb, args);
                    return;
                }
            }
        }
    }

    private Map<String, Object> leaveWorld() throws Exception {
        final Object mc = g.requireMc();
        ((Executor) mc).execute(new Runnable() {
            @Override
            public void run() {
                try {
                    Object level = g.get(mc, "level");
                    if (g.has(mc, "disconnectWithSavingScreen", 0)) {
                        g.call(mc, "disconnectWithSavingScreen");
                    } else {
                        if (level != null && g.has(level, "disconnect", 0)) g.call(level, "disconnect");
                        if (g.has(mc, "disconnect", 0)) g.call(mc, "disconnect");
                        else g.call(mc, "clearLevel");
                    }
                    setScreen(mc, null);
                } catch (Throwable t) {
                    Log.error("leave_world failed", t);
                }
            }
        });
        return ok();
    }

    // ------------------------------------------------------------------ helpers

    static Map<String, Object> ok() {
        Map<String, Object> o = new LinkedHashMap<String, Object>();
        o.put("ok", true);
        return o;
    }

    static String stripSlash(String c) {
        if (c == null) throw new AgentException("missing 'command'");
        c = c.trim();
        return c.startsWith("/") ? c.substring(1) : c;
    }

    static String str(Map<String, Object> p, String key, String def) {
        Object v = p.get(key);
        if (v != null) return v.toString();
        if (def == null) throw new AgentException("missing '" + key + "'");
        return def;
    }
}
