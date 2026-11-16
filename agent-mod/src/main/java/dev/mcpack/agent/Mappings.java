package dev.mcpack.agent;

import java.io.BufferedReader;
import java.io.FileInputStream;
import java.io.InputStreamReader;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * Named (Mojang, or MCP for old Forge) → runtime names, written by the launcher:
 * <pre>
 * mcpack-mappings 1 &lt;namespace&gt;
 * C &lt;named class&gt; &lt;runtime class&gt;
 * M &lt;named method&gt; &lt;runtime method&gt; &lt;named param types, comma-separated, or -&gt;
 * F &lt;named field&gt; &lt;runtime field&gt;
 * </pre>
 * Members belong to the preceding C line. Anything not listed keeps its name.
 */
final class Mappings {
    static final Mappings IDENTITY = new Mappings("identity");

    final String namespace;
    private final Map<String, String> classes = new HashMap<String, String>();
    private final Map<String, String> reverseClasses = new HashMap<String, String>();
    private final Map<String, Map<String, List<String[]>>> methods = new HashMap<String, Map<String, List<String[]>>>();
    private final Map<String, Map<String, String>> fields = new HashMap<String, Map<String, String>>();

    private Mappings(String namespace) {
        this.namespace = namespace;
    }

    static Mappings load(String path) {
        if (path == null || path.trim().isEmpty()) return IDENTITY;
        BufferedReader r = null;
        try {
            r = new BufferedReader(new InputStreamReader(new FileInputStream(path), "UTF-8"), 1 << 16);
            String header = r.readLine();
            String[] h = header == null ? new String[0] : header.split(" ");
            Mappings m = new Mappings(h.length > 2 ? h[2] : "unknown");
            String owner = null;
            String line;
            while ((line = r.readLine()) != null) {
                if (line.isEmpty()) continue;
                String[] p = line.split(" ");
                switch (p[0]) {
                    case "C":
                        owner = p[1];
                        m.classes.put(p[1], p[2]);
                        m.reverseClasses.put(p[2], p[1]);
                        break;
                    case "M": {
                        Map<String, List<String[]>> byName = m.methods.get(owner);
                        if (byName == null) {
                            byName = new HashMap<String, List<String[]>>();
                            m.methods.put(owner, byName);
                        }
                        List<String[]> l = byName.get(p[1]);
                        if (l == null) {
                            l = new ArrayList<String[]>(1);
                            byName.put(p[1], l);
                        }
                        l.add(new String[] {p[2], p.length > 3 ? p[3] : "-"});
                        break;
                    }
                    case "F": {
                        Map<String, String> f = m.fields.get(owner);
                        if (f == null) {
                            f = new HashMap<String, String>();
                            m.fields.put(owner, f);
                        }
                        f.put(p[1], p[2]);
                        break;
                    }
                    default:
                        break;
                }
            }
            Log.info("loaded " + m.classes.size() + " class mappings (" + m.namespace + ")");
            return m;
        } catch (Exception e) {
            Log.error("cannot read mappings " + path + "; assuming named runtime", e);
            return IDENTITY;
        } finally {
            if (r != null) {
                try {
                    r.close();
                } catch (Exception ignored) {
                }
            }
        }
    }

    boolean isIdentity() {
        return classes.isEmpty();
    }

    String runtimeClass(String named) {
        String r = classes.get(named);
        return r == null ? named : r;
    }

    String namedClass(String runtime) {
        String n = reverseClasses.get(runtime);
        return n == null ? runtime : n;
    }

    /** Runtime names (with named param lists) of a named method declared in {@code namedOwner}. */
    List<String[]> methods(String namedOwner, String named) {
        Map<String, List<String[]>> byName = methods.get(namedOwner);
        List<String[]> l = byName == null ? null : byName.get(named);
        if (l == null) return Collections.singletonList(new String[] {named, null});
        return l;
    }

    /** Named name of a runtime method of {@code namedOwner} (for proxies). */
    String namedMethod(String namedOwner, String runtime, int paramCount) {
        Map<String, List<String[]>> byName = methods.get(namedOwner);
        if (byName == null) return runtime;
        for (Map.Entry<String, List<String[]>> e : byName.entrySet()) {
            for (String[] v : e.getValue()) {
                if (v[0].equals(runtime) && paramCount(v[1]) == paramCount) return e.getKey();
            }
        }
        return runtime;
    }

    String field(String namedOwner, String named) {
        Map<String, String> f = fields.get(namedOwner);
        String r = f == null ? null : f.get(named);
        return r == null ? named : r;
    }

    static int paramCount(String params) {
        if (params == null || params.equals("-") || params.isEmpty()) return 0;
        int n = 1;
        for (int i = 0; i < params.length(); i++) if (params.charAt(i) == ',') n++;
        return n;
    }
}
