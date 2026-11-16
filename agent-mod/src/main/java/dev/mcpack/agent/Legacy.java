package dev.mcpack.agent;

import java.io.File;
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.Callable;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Agent methods for Forge on Minecraft 1.7.10 – 1.12.2, written with MCP names (the launcher's
 * mappings translate them to the SRG names the game runs with).
 */
final class Legacy {
    private final Game g;

    Legacy(Game g) {
        this.g = g;
    }

    Object dispatch(String method, final Map<String, Object> p) throws Exception {
        switch (method) {
            case "status":
                return status();
            case "command":
                return command(Handlers.str(p, "command", null));
            case "client_command": {
                final String cmd = Handlers.stripSlash(Handlers.str(p, "command", null));
                return sendChat("/" + cmd);
            }
            case "chat":
                return sendChat(Handlers.str(p, "message", null));
            case "reload_resources":
                return reloadResources(p);
            case "reload_datapacks":
                throw new AgentException("data packs need Minecraft 1.13 or newer");
            case "screenshot":
                return screenshot(p);
            case "join_world":
                return joinWorld(Handlers.str(p, "level_id", null));
            case "leave_world":
                return leaveWorld();
            case "exit":
                schedule(new Runnable() {
                    @Override
                    public void run() {
                        g.call(g.requireMc(), "shutdown");
                    }
                });
                return Handlers.ok();
            default:
                throw new AgentException("unknown method: " + method);
        }
    }

    // ------------------------------------------------------------------ helpers

    /** The first of several field names that exists (names changed between versions). */
    private Object field(Object target, String... names) {
        for (String n : names) {
            if (g.hasField(target, n)) return g.get(target, n);
        }
        return null;
    }

    private Object world(Object mc) {
        return field(mc, "world", "theWorld");
    }

    private Object player(Object mc) {
        return field(mc, "player", "thePlayer");
    }

    private String text(Object component) {
        if (component == null) return null;
        try {
            return String.valueOf(g.callFirst(component, new String[] {"getUnformattedText", "getFormattedText"}));
        } catch (RuntimeException e) {
            return component.toString();
        }
    }

    private void schedule(Runnable r) {
        g.callFirst(g.requireMc(), new String[] {"addScheduledTask", "func_152344_a"}, r);
    }

    /** Run on a legacy game thread (client or integrated server) and wait. */
    private <T> T on(Object threadOwner, final Callable<T> task) throws Exception {
        if (!g.has(threadOwner, "addScheduledTask", 1) && !g.has(threadOwner, "func_152344_a", 1)) {
            // 1.7.x servers have no task queue; RCON runs commands on its own thread the same way.
            synchronized (threadOwner) {
                return task.call();
            }
        }
        final AtomicReference<T> result = new AtomicReference<T>();
        final AtomicReference<Throwable> error = new AtomicReference<Throwable>();
        final CountDownLatch done = new CountDownLatch(1);
        g.callFirst(threadOwner, new String[] {"addScheduledTask", "func_152344_a"}, new Runnable() {
            @Override
            public void run() {
                try {
                    result.set(task.call());
                } catch (Throwable t) {
                    error.set(t);
                } finally {
                    done.countDown();
                }
            }
        });
        if (!done.await(60, TimeUnit.SECONDS)) throw new AgentException("timed out waiting for the game thread");
        Throwable t = error.get();
        if (t instanceof Exception) throw (Exception) t;
        if (t instanceof Error) throw (Error) t;
        return result.get();
    }

    private static boolean isLoadingScreen(String name) {
        String simple = name.substring(name.lastIndexOf('.') + 1);
        return simple.equals("GuiDownloadTerrain") || simple.equals("GuiScreenWorking") || simple.equals("GuiConnecting")
                || simple.equals("GuiScreenDemo") || simple.equals("GuiMemoryErrorScreen");
    }

    // ------------------------------------------------------------------ status

    private Map<String, Object> status() throws Exception {
        if (g.mc() == null) {
            Map<String, Object> o = new LinkedHashMap<String, Object>();
            o.put("minecraft", System.getProperty("mcpack.agent.mc"));
            o.put("loader", Handlers.loaderName());
            o.put("loading", true);
            o.put("title_screen", false);
            o.put("in_world", false);
            o.put("starting", true);
            return o;
        }
        final Object mc = g.requireMc();
        return on(mc, new Callable<Map<String, Object>>() {
            @Override
            public Map<String, Object> call() {
                Map<String, Object> o = new LinkedHashMap<String, Object>();
                o.put("minecraft", System.getProperty("mcpack.agent.mc"));
                o.put("loader", Handlers.loaderName());
                o.put("mods_loaded", modCount());
                Object screen = g.get(mc, "currentScreen");
                String screenName = screen == null ? null : screen.getClass().getName();
                o.put("screen", screenName);
                o.put("screen_title", null);
                o.put("title_screen", g.isInstance("net.minecraft.client.gui.GuiMainMenu", screen));
                o.put("loading", screenName != null && isLoadingScreen(screenName));
                Object world = world(mc);
                Object player = player(mc);
                o.put("in_world", world != null && player != null);
                o.put("singleplayer", g.call(mc, "isSingleplayer"));
                Object fps = g.has(mc.getClass(), "getDebugFPS", 0) ? g.callStatic(mc.getClass(), "getDebugFPS") : g.getOrNull(mc.getClass(), "debugFPS");
                o.put("fps", fps);
                Object server = g.call(mc, "getIntegratedServer");
                if (server != null) {
                    try {
                        String folder = String.valueOf(g.call(server, "getFolderName"));
                        o.put("level_id", folder);
                        o.put("world_dir", new File((File) field(mc, "gameDir", "mcDataDir"), "saves/" + folder).getAbsolutePath());
                        o.put("world_name", g.call(server, "getWorldName"));
                    } catch (RuntimeException e) {
                        o.put("world_error", e.toString());
                    }
                }
                if (player != null && world != null) {
                    Map<String, Object> pos = new LinkedHashMap<String, Object>();
                    pos.put("x", g.get(player, "posX"));
                    pos.put("y", g.get(player, "posY"));
                    pos.put("z", g.get(player, "posZ"));
                    o.put("position", pos);
                    Object dim = g.getOrNull(player, "dimension");
                    if (dim instanceof Integer) {
                        int d = (Integer) dim;
                        o.put("dimension", d == 0 ? "minecraft:overworld" : d == -1 ? "minecraft:the_nether" : d == 1 ? "minecraft:the_end" : "dimension:" + d);
                    }
                    o.put("health", g.call(player, "getHealth"));
                }
                return o;
            }
        });
    }

    private Integer modCount() {
        ClassLoader cl = g.requireMc().getClass().getClassLoader();
        for (String loaderClass : new String[] {"net.minecraftforge.fml.common.Loader", "cpw.mods.fml.common.Loader"}) {
            try {
                Class<?> c = Class.forName(loaderClass, true, cl);
                Object inst = c.getMethod("instance").invoke(null);
                Object mods = c.getMethod("getModList").invoke(inst);
                if (mods instanceof List) return ((List<?>) mods).size();
            } catch (Throwable ignored) {
            }
        }
        return null;
    }

    // ------------------------------------------------------------------ commands

    private Map<String, Object> command(String raw) throws Exception {
        final String cmd = Handlers.stripSlash(raw);
        final Object mc = g.requireMc();
        final Object server = g.call(mc, "getIntegratedServer");
        if (server == null) throw new AgentException("command requires a singleplayer world (use world_enter first)");
        final List<String> output = Collections.synchronizedList(new ArrayList<String>());
        on(server, new Callable<Object>() {
            @Override
            @SuppressWarnings("unchecked")
            public Object call() {
                Object players = g.has(server, "getPlayerList", 0) ? g.call(server, "getPlayerList") : g.call(server, "getConfigurationManager");
                List<Object> list = (List<Object>) g.get(players, "playerEntityList");
                if (list.isEmpty()) throw new AgentException("no player on the integrated server yet");
                Object sender = captureSender(list.get(0), output);
                g.call(g.call(server, "getCommandManager"), "executeCommand", sender, cmd);
                return null;
            }
        });
        Map<String, Object> o = new LinkedHashMap<String, Object>();
        synchronized (output) {
            o.put("output", new ArrayList<String>(output));
        }
        return o;
    }

    /** An ICommandSender acting as the player, with operator rights, recording messages. */
    private Object captureSender(final Object player, final List<String> output) {
        final Class<?> iface = g.cls("net.minecraft.command.ICommandSender");
        final Class<?> chat = componentClass();
        return Proxy.newProxyInstance(iface.getClassLoader(), new Class<?>[] {iface}, new InvocationHandler() {
            @Override
            public Object invoke(Object proxy, Method m, Object[] args) throws Throwable {
                if (m.getDeclaringClass() == Object.class) {
                    if (m.getName().equals("equals")) return proxy == args[0];
                    if (m.getName().equals("hashCode")) return System.identityHashCode(proxy);
                    return "mcpack-agent";
                }
                if (args != null) {
                    for (Object a : args) {
                        if (chat != null && chat.isInstance(a)) {
                            output.add(text(a));
                            return null;
                        }
                    }
                }
                String named = g.mappings().namedMethod("net.minecraft.command.ICommandSender", m.getName(), m.getParameterTypes().length);
                if (named.equals("canCommandSenderUseCommand") || named.equals("canUseCommand") || named.equals("sendCommandFeedback")) {
                    return true;
                }
                try {
                    m.setAccessible(true);
                    return m.invoke(player, args);
                } catch (InvocationTargetException e) {
                    throw e.getCause() == null ? e : e.getCause();
                }
            }
        });
    }

    private Class<?> componentClass() {
        Class<?> c = g.clsOrNull("net.minecraft.util.text.ITextComponent");
        return c != null ? c : g.clsOrNull("net.minecraft.util.IChatComponent");
    }

    private Map<String, Object> sendChat(final String message) throws Exception {
        final Object mc = g.requireMc();
        on(mc, new Callable<Object>() {
            @Override
            public Object call() {
                Object player = player(mc);
                if (player == null) throw new AgentException("not in a world");
                return g.call(player, "sendChatMessage", message);
            }
        });
        return Handlers.ok();
    }

    // ------------------------------------------------------------------ resources

    @SuppressWarnings("unchecked")
    private Map<String, Object> reloadResources(Map<String, Object> p) throws Exception {
        final Set<String> enable = new LinkedHashSet<String>();
        Object e = p.get("enable");
        if (e instanceof List) {
            for (Object x : (List<Object>) e) {
                String id = String.valueOf(x);
                enable.add(id.startsWith("file/") ? id.substring(5) : id);
            }
        }
        final Object mc = g.requireMc();
        Object selected = on(mc, new Callable<Object>() {
            @Override
            public Object call() {
                Object repo = g.call(mc, "getResourcePackRepository");
                g.call(repo, "updateRepositoryEntriesAll");
                Object settings = g.get(mc, "gameSettings");
                List<String> names = (List<String>) g.get(settings, "resourcePacks");
                if (!enable.isEmpty()) {
                    List<Object> chosen = new ArrayList<Object>((List<Object>) g.call(repo, "getRepositoryEntries"));
                    for (Object entry : (List<Object>) g.call(repo, "getRepositoryEntriesAll")) {
                        String name = String.valueOf(g.call(entry, "getResourcePackName"));
                        if (enable.contains(name) && !chosen.contains(entry)) {
                            chosen.add(entry);
                            if (!names.contains(name)) names.add(name);
                        }
                    }
                    g.call(repo, "setRepositories", chosen);
                    g.call(settings, "saveOptions");
                }
                g.call(mc, "refreshResources");
                List<String> out = new ArrayList<String>();
                for (String n : names) out.add("file/" + n);
                return out;
            }
        });
        Map<String, Object> o = Handlers.ok();
        o.put("selected", selected);
        return o;
    }

    // ------------------------------------------------------------------ screenshots

    private Map<String, Object> screenshot(Map<String, Object> p) throws Exception {
        final Object mc = g.requireMc();
        final String name = Handlers.str(p, "name", "mcpack-" + System.currentTimeMillis()) + ".png";
        final File dir = (File) field(mc, "gameDir", "mcDataDir");
        on(mc, new Callable<Object>() {
            @Override
            public Object call() {
                Class<?> helper = g.cls("net.minecraft.util.ScreenShotHelper");
                Object fb = g.has(mc, "getFramebuffer", 0) ? g.call(mc, "getFramebuffer") : null;
                for (Method m : g.methods(helper, "saveScreenshot")) {
                    Class<?>[] ps = m.getParameterTypes();
                    boolean named = false;
                    for (Class<?> c : ps) if (c == String.class) named = true;
                    if (!named) continue;
                    Object[] args = new Object[ps.length];
                    int ints = 0;
                    for (int i = 0; i < ps.length; i++) {
                        Class<?> c = ps[i];
                        if (c == File.class) args[i] = dir;
                        else if (c == String.class) args[i] = name;
                        else if (c == int.class) args[i] = g.get(mc, ints++ == 0 ? "displayWidth" : "displayHeight");
                        else if (fb != null && c.isInstance(fb)) args[i] = fb;
                        else args[i] = null;
                    }
                    return g.invoke(m, null, args);
                }
                throw new AgentException("no ScreenShotHelper.saveScreenshot with a file name");
            }
        });
        File f = new File(new File(dir, "screenshots"), name);
        long end = System.currentTimeMillis() + 15000;
        while (!(f.isFile() && f.length() > 0) && System.currentTimeMillis() < end) Thread.sleep(100);
        if (!f.isFile()) throw new AgentException("screenshot was not written");
        Map<String, Object> o = Handlers.ok();
        o.put("path", f.getAbsolutePath());
        return o;
    }

    // ------------------------------------------------------------------ worlds

    private Map<String, Object> joinWorld(final String levelId) {
        final Object mc = g.requireMc();
        schedule(new Runnable() {
            @Override
            public void run() {
                try {
                    g.call(mc, "displayGuiScreen", (Object) null);
                    g.call(mc, "launchIntegratedServer", levelId, levelId, null);
                } catch (Throwable t) {
                    Log.error("join_world " + levelId + " failed", t);
                }
            }
        });
        return Handlers.ok();
    }

    private Map<String, Object> leaveWorld() {
        final Object mc = g.requireMc();
        schedule(new Runnable() {
            @Override
            public void run() {
                try {
                    Object world = world(mc);
                    if (world != null) g.call(world, "sendQuittingDisconnectingPacket");
                    g.call(mc, "loadWorld", (Object) null);
                    g.call(mc, "displayGuiScreen", g.construct(g.cls("net.minecraft.client.gui.GuiMainMenu")));
                } catch (Throwable t) {
                    Log.error("leave_world failed", t);
                }
            }
        });
        return Handlers.ok();
    }
}
