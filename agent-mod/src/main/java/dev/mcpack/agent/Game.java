package dev.mcpack.agent;

import java.lang.instrument.Instrumentation;
import java.lang.reflect.AccessibleObject;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Member;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Deque;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.Callable;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Executor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Finds the running game and calls into it by named (Mojang/MCP) member names, whatever the
 * runtime names are (obfuscated, Fabric intermediary, Forge SRG, or already Mojang names).
 */
final class Game {
    static final String MINECRAFT = "net.minecraft.client.Minecraft";

    private final Instrumentation inst;
    private volatile Mappings map;
    private volatile Class<?> mcClass;
    private volatile Object mc;
    private ClassLoader loader;
    private final Map<Class<?>, Method[]> declared = new HashMap<Class<?>, Method[]>();
    private final Set<String> openedPackages = Collections.synchronizedSet(new HashSet<String>());
    /** Legacy (pre-1.13 Forge, MCP names) versus modern (Mojang names). */
    private final boolean legacy;

    Game(Instrumentation inst, Mappings map) {
        this.inst = inst;
        this.map = map;
        this.legacy = "mcp".equals(map.namespace);
    }

    boolean legacy() {
        return legacy;
    }

    Mappings mappings() {
        return map;
    }

    // ------------------------------------------------------------------ locating the game

    private Class<?> findLoaded(String name) {
        for (Class<?> c : inst.getAllLoadedClasses()) {
            if (c.getName().equals(name)) return c;
        }
        return null;
    }

    private static boolean declares(Class<?> c, String method) {
        try {
            for (Method m : c.getDeclaredMethods()) {
                if (m.getName().equals(method)) return true;
            }
        } catch (Throwable ignored) {
        }
        return false;
    }

    /** The Minecraft client instance, or null while the game is still starting. */
    synchronized Object mc() {
        if (mc != null) {
            useGameLoader();
            return mc;
        }
        if (mcClass == null) {
            String getter = legacy ? "getMinecraft" : "getInstance";
            Class<?> c = findLoaded(map.runtimeClass(MINECRAFT));
            boolean maybeNamed = false;
            if (c == null && !map.isIdentity()) {
                c = findLoaded(MINECRAFT);
                maybeNamed = c != null;
            }
            if (c == null) return null;
            // Touching game classes while the game starts can load them on our thread before
            // the loader is ready for that (Forge then loads them untransformed). Wait, passively,
            // until the client's main loop runs and mod loading has finished.
            if (!startedUp(c)) return null;
            loader = c.getClassLoader();
            useGameLoader();
            // The runtime may use named names although we were given mappings (e.g. NeoForge
            // 1.20.5+, Forge 1.20.6+): detect it instead of trusting the launcher's guess.
            if (!map.isIdentity() && (maybeNamed || c.getName().equals(MINECRAFT)) && declares(c, getter)) {
                Log.info("runtime uses named names; ignoring mappings");
                map = Mappings.IDENTITY;
            } else if (maybeNamed) {
                return null;
            }
            mcClass = c;
            loader = c.getClassLoader();
            Log.info("found " + c.getName() + " (" + map.namespace + " names)");
        }
        useGameLoader();
        try {
            Object o = legacy ? callStatic(mcClass, "getMinecraft") : callStatic(mcClass, "getInstance");
            if (o != null) mc = o;
            return o;
        } catch (Throwable e) {
            return null;
        }
    }

    /** Forge-family classes whose static isLoading() is true until mods have finished loading. */
    private static final String[] MOD_LOADING_FLAGS = {
        "net.minecraftforge.client.loading.ClientModLoader",
        "net.minecraftforge.fml.client.ClientModLoader",
        "net.neoforged.neoforge.client.loading.ClientModLoader",
        "net.neoforged.fml.loading.progress.StartupNotificationManager",
    };

    private boolean startedUp;

    /** Has the client left its constructor and entered the main loop, with mods loaded? */
    private boolean startedUp(Class<?> c) {
        if (startedUp) return true;
        String cn = c.getName();
        Set<String> loop = new HashSet<String>();
        Set<String> init = new HashSet<String>();
        init.add("<init>");
        for (String named : new String[] {"runTick", "runGameLoop"}) {
            loop.add(named);
            for (String[] m : map.methods(MINECRAFT, named)) loop.add(m[0]);
        }
        for (String named : new String[] {"init", "startGame"}) {
            if (!legacy) break;
            for (String[] m : map.methods(MINECRAFT, named)) init.add(m[0]);
        }
        boolean inLoop = false;
        for (StackTraceElement[] st : Thread.getAllStackTraces().values()) {
            boolean looping = false;
            boolean constructing = false;
            for (StackTraceElement e : st) {
                if (!e.getClassName().equals(cn)) continue;
                if (init.contains(e.getMethodName())) constructing = true;
                else if (loop.contains(e.getMethodName())) looping = true;
            }
            if (looping && !constructing) inLoop = true;
        }
        if (!inLoop) return false;
        for (String name : MOD_LOADING_FLAGS) {
            Class<?> k = findLoaded(name);
            if (k == null) continue;
            try {
                Thread.currentThread().setContextClassLoader(k.getClassLoader());
                Method m = k.getDeclaredMethod("isLoading");
                m.setAccessible(true);
                if (Boolean.TRUE.equals(m.invoke(null))) return false;
            } catch (NoSuchMethodException ignored) {
            } catch (Throwable e) {
                return false;
            }
        }
        startedUp = true;
        return true;
    }

    /**
     * Loaders resolve classes through the thread context class loader (Forge's event bus
     * transformer does, for one); on our threads it must be the game's, as on the game's own.
     */
    private void useGameLoader() {
        if (loader != null && Thread.currentThread().getContextClassLoader() != loader) {
            Thread.currentThread().setContextClassLoader(loader);
        }
    }

    Object requireMc() {
        Object o = mc();
        if (o == null) throw new AgentException("the game is still starting");
        return o;
    }

    // ------------------------------------------------------------------ classes

    Class<?> cls(String named) {
        Class<?> c = clsOrNull(named);
        if (c == null) throw new AgentException("class not found: " + named + " (" + map.runtimeClass(named) + ")");
        return c;
    }

    Class<?> clsOrNull(String named) {
        requireMc();
        String rt = map.runtimeClass(named);
        try {
            return Class.forName(rt, false, loader);
        } catch (Throwable e) {
            return null;
        }
    }

    boolean isInstance(String named, Object o) {
        Class<?> c = clsOrNull(named);
        return c != null && o != null && c.isInstance(o);
    }

    String namedClassOf(Class<?> c) {
        return map.namedClass(c.getName());
    }

    // ------------------------------------------------------------------ members

    private static List<Class<?>> hierarchy(Class<?> c) {
        List<Class<?>> out = new ArrayList<Class<?>>();
        Set<Class<?>> seen = new LinkedHashSet<Class<?>>();
        Deque<Class<?>> q = new ArrayDeque<Class<?>>();
        q.add(c);
        while (!q.isEmpty()) {
            Class<?> k = q.poll();
            if (k == null || !seen.add(k)) continue;
            out.add(k);
            if (k.getSuperclass() != null) q.add(k.getSuperclass());
            Collections.addAll(q, k.getInterfaces());
        }
        return out;
    }

    private Method[] declaredMethods(Class<?> k) {
        synchronized (declared) {
            Method[] ms = declared.get(k);
            if (ms == null) {
                try {
                    ms = k.getDeclaredMethods();
                } catch (Throwable e) {
                    ms = new Method[0];
                }
                declared.put(k, ms);
            }
            return ms;
        }
    }

    private static Class<?> box(Class<?> p) {
        if (!p.isPrimitive()) return p;
        if (p == int.class) return Integer.class;
        if (p == long.class) return Long.class;
        if (p == boolean.class) return Boolean.class;
        if (p == double.class) return Double.class;
        if (p == float.class) return Float.class;
        if (p == short.class) return Short.class;
        if (p == byte.class) return Byte.class;
        if (p == char.class) return Character.class;
        return Void.class;
    }

    private static boolean compatible(Class<?>[] params, Object[] args) {
        if (args == null) return true;
        if (params.length != args.length) return false;
        for (int i = 0; i < params.length; i++) {
            Object a = args[i];
            if (a == null) {
                if (params[i].isPrimitive()) return false;
            } else if (!box(params[i]).isInstance(a)) {
                return false;
            }
        }
        return true;
    }

    private static String typeName(Class<?> c) {
        if (c.isArray()) return typeName(c.getComponentType()) + "[]";
        return c.getName();
    }

    private boolean paramsMatch(Method m, String namedParams) {
        if (namedParams == null) return true;
        Class<?>[] ps = m.getParameterTypes();
        if (Mappings.paramCount(namedParams) != ps.length) return false;
        if (ps.length == 0) return true;
        String[] names = namedParams.split(",");
        for (int i = 0; i < ps.length; i++) {
            String want = names[i];
            int dims = 0;
            while (want.endsWith("[]")) {
                want = want.substring(0, want.length() - 2);
                dims++;
            }
            StringBuilder rt = new StringBuilder(map.runtimeClass(want));
            for (int d = 0; d < dims; d++) rt.append("[]");
            if (!rt.toString().equals(typeName(ps[i]))) return false;
        }
        return true;
    }

    /** All methods with this named name (any arity), most specific class first. */
    List<Method> methods(Class<?> c, String named) {
        List<Method> out = new ArrayList<Method>();
        for (Class<?> k : hierarchy(c)) {
            String owner = map.namedClass(k.getName());
            for (String[] cand : map.methods(owner, named)) {
                for (Method m : declaredMethods(k)) {
                    if (m.getName().equals(cand[0]) && paramsMatch(m, cand[1]) && !out.contains(m)) out.add(m);
                }
            }
        }
        return out;
    }

    Method find(Class<?> c, String named, Object[] args) {
        for (Method m : methods(c, named)) {
            if (m.isBridge()) continue;
            if (compatible(m.getParameterTypes(), args)) return m;
        }
        for (Method m : methods(c, named)) {
            if (compatible(m.getParameterTypes(), args)) return m;
        }
        return null;
    }

    boolean has(Object target, String named, int argc) {
        if (target == null) return false;
        Class<?> c = target instanceof Class ? (Class<?>) target : target.getClass();
        for (Method m : methods(c, named)) {
            if (argc < 0 || m.getParameterTypes().length == argc) return true;
        }
        return false;
    }

    Object call(Object target, String named, Object... args) {
        if (target == null) throw new AgentException("call " + named + " on null");
        Method m = find(target.getClass(), named, args);
        if (m == null) {
            throw new AgentException("no method " + namedClassOf(target.getClass()) + "." + named + "/" + args.length);
        }
        return invoke(m, target, args);
    }

    /** Call the first of several named methods that exists with these arguments. */
    Object callFirst(Object target, String[] names, Object... args) {
        for (String n : names) {
            Method m = find(target.getClass(), n, args);
            if (m != null) return invoke(m, target, args);
        }
        throw new AgentException("none of " + java.util.Arrays.toString(names) + " on " + namedClassOf(target.getClass()));
    }

    Object callStatic(Class<?> c, String named, Object... args) {
        Method m = find(c, named, args);
        if (m == null) throw new AgentException("no static method " + namedClassOf(c) + "." + named + "/" + args.length);
        return invoke(m, null, args);
    }

    Object invoke(Method m, Object target, Object... args) {
        access(m);
        try {
            return m.invoke(target, args);
        } catch (InvocationTargetException e) {
            Throwable t = e.getCause() == null ? e : e.getCause();
            if (t instanceof RuntimeException) throw (RuntimeException) t;
            if (t instanceof Error) throw (Error) t;
            throw new RuntimeException(t);
        } catch (IllegalAccessException e) {
            throw new AgentException("cannot call " + m + ": " + e.getMessage());
        }
    }

    Field field(Class<?> c, String named) {
        for (Class<?> k = c; k != null; k = k.getSuperclass()) {
            String rt = map.field(map.namedClass(k.getName()), named);
            try {
                return k.getDeclaredField(rt);
            } catch (NoSuchFieldException ignored) {
            } catch (Throwable ignored) {
                return null;
            }
        }
        return null;
    }

    boolean hasField(Object target, String named) {
        if (target == null) return false;
        return field(target instanceof Class ? (Class<?>) target : target.getClass(), named) != null;
    }

    Object get(Object target, String named) {
        if (target == null) throw new AgentException("read " + named + " of null");
        Class<?> c = target instanceof Class ? (Class<?>) target : target.getClass();
        Field f = field(c, named);
        if (f == null) throw new AgentException("no field " + namedClassOf(c) + "." + named);
        access(f);
        try {
            return f.get(Modifier.isStatic(f.getModifiers()) ? null : target);
        } catch (IllegalAccessException e) {
            throw new AgentException("cannot read " + f + ": " + e.getMessage());
        }
    }

    Object getOrNull(Object target, String named) {
        try {
            return hasField(target, named) ? get(target, named) : null;
        } catch (RuntimeException e) {
            return null;
        }
    }

    void set(Object target, String named, Object value) {
        Field f = field(target.getClass(), named);
        if (f == null) throw new AgentException("no field " + namedClassOf(target.getClass()) + "." + named);
        access(f);
        try {
            f.set(target, value);
        } catch (IllegalAccessException e) {
            throw new AgentException("cannot write " + f + ": " + e.getMessage());
        }
    }

    Object construct(Class<?> c, Object... args) {
        for (Constructor<?> k : c.getDeclaredConstructors()) {
            if (compatible(k.getParameterTypes(), args)) {
                access(k);
                try {
                    return k.newInstance(args);
                } catch (InvocationTargetException e) {
                    Throwable t = e.getCause() == null ? e : e.getCause();
                    if (t instanceof RuntimeException) throw (RuntimeException) t;
                    throw new RuntimeException(t);
                } catch (Exception e) {
                    throw new AgentException("cannot construct " + c.getName() + ": " + e);
                }
            }
        }
        throw new AgentException("no constructor " + namedClassOf(c) + "/" + args.length);
    }

    /** setAccessible, opening the package to us first when the game runs in a named module. */
    private void access(AccessibleObject o) {
        try {
            o.setAccessible(true);
        } catch (RuntimeException e) {
            Class<?> owner = ((Member) o).getDeclaringClass();
            if (openPackage(owner)) {
                o.setAccessible(true);
            } else {
                throw e;
            }
        }
    }

    /** Java 9+: open the package of {@code c} to this agent via Instrumentation.redefineModule. */
    private boolean openPackage(Class<?> c) {
        try {
            Method getModule = Class.class.getMethod("getModule");
            Object module = getModule.invoke(c);
            Object self = getModule.invoke(Game.class);
            String pkg = c.getName().substring(0, Math.max(0, c.getName().lastIndexOf('.')));
            if (!openedPackages.add(module + "/" + pkg)) return true;
            Class<?> moduleClass = Class.forName("java.lang.Module");
            Map<String, Set<Object>> opens = new HashMap<String, Set<Object>>();
            opens.put(pkg, Collections.singleton(self));
            Method redefine = Instrumentation.class.getMethod("redefineModule", moduleClass, Set.class, Map.class, Map.class, Set.class, Map.class);
            redefine.invoke(inst, module, Collections.emptySet(), Collections.emptyMap(), opens, Collections.emptySet(), Collections.emptyMap());
            return true;
        } catch (Throwable e) {
            return false;
        }
    }

    // ------------------------------------------------------------------ threads

    /** Run on the game's client thread and wait for the result. */
    <T> T onClient(final Callable<T> task) throws Exception {
        final Object m = requireMc();
        Executor ex = m instanceof Executor ? (Executor) m : new Executor() {
            @Override
            public void execute(Runnable r) {
                // Pre-1.14 clients: scheduled tasks instead of an executor.
                callFirst(m, new String[] {"addScheduledTask", "func_152344_a"}, r);
            }
        };
        return onThread(ex, task, 60);
    }

    <T> T onThread(Executor executor, final Callable<T> task, int timeoutSecs) throws Exception {
        final AtomicReference<T> result = new AtomicReference<T>();
        final AtomicReference<Throwable> error = new AtomicReference<Throwable>();
        final CountDownLatch done = new CountDownLatch(1);
        executor.execute(new Runnable() {
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
        if (!done.await(timeoutSecs, TimeUnit.SECONDS)) throw new AgentException("timed out waiting for the game thread");
        Throwable t = error.get();
        if (t instanceof Exception) throw (Exception) t;
        if (t instanceof Error) throw (Error) t;
        return result.get();
    }
}
