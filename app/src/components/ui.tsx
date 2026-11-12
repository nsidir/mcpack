import { type ButtonHTMLAttributes, type CSSProperties, type ReactNode, useEffect, useRef, useState } from "react";
import { X } from "lucide-react";

type BtnProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "default" | "primary" | "ghost" | "danger";
  size?: "sm" | "md" | "lg";
  icon?: boolean;
};

export function Button({ variant = "default", size = "md", icon, className = "", type = "button", ...rest }: BtnProps) {
  const cls = ["btn", variant !== "default" && variant, size !== "md" && size, icon && "icon", className].filter(Boolean).join(" ");
  return <button type={type} className={cls} {...rest} />;
}

/** Icon-only button; `label` is both the tooltip and the accessible name. */
export function IconButton({ label, ...rest }: BtnProps & { label: string }) {
  return <Button icon variant="ghost" title={label} aria-label={label} {...rest} />;
}

export function Badge({ tone, children }: { tone?: "ok" | "warn" | "error" | "ai"; children: ReactNode }) {
  return <span className={`badge ${tone ?? ""}`}>{children}</span>;
}

export function Dot({ tone, pulse }: { tone?: "ok" | "warn" | "error"; pulse?: boolean }) {
  return <span className={`dot ${tone ?? ""} ${pulse ? "pulse" : ""}`} aria-hidden />;
}

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className="kbd">{children}</kbd>;
}

export function Switch({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label: string }) {
  return <button type="button" role="switch" className="switch" aria-checked={checked} aria-label={label} onClick={() => onChange(!checked)} />;
}

export function Segmented<T extends string>({
  value,
  onChange,
  options,
  label,
}: {
  value: T;
  onChange: (v: T) => void;
  options: { value: T; label: string }[];
  label: string;
}) {
  return (
    <div className="seg" role="group" aria-label={label}>
      {options.map((o) => (
        <button key={o.value} type="button" aria-pressed={o.value === value} onClick={() => onChange(o.value)}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function Empty({ icon, title, children, action }: { icon: ReactNode; title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="empty">
      {icon}
      <h3>{title}</h3>
      {children && <p>{children}</p>}
      {action}
    </div>
  );
}

/** Native <dialog>: focus trap, Esc and inert background come for free. */
export function Modal({
  open,
  onClose,
  label,
  title,
  children,
  style,
}: {
  open: boolean;
  onClose: () => void;
  label: string;
  title?: string;
  children: ReactNode;
  style?: CSSProperties;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) d.showModal();
    if (!open && d.open) d.close();
  }, [open]);
  return (
    <dialog
      ref={ref}
      className="modal"
      aria-label={label}
      style={style}
      onClose={onClose}
      onMouseDown={(e) => e.target === ref.current && onClose()}
    >
      {open && (
        <>
          {title && (
            <div className="modal-head">
              <h2>{title}</h2>
              <IconButton label="Close" onClick={onClose}>
                <X size={15} />
              </IconButton>
            </div>
          )}
          {children}
        </>
      )}
    </dialog>
  );
}

// ---------------------------------------------------------------- helpers

export function useNow(intervalMs: number, active = true) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!active) return;
    const t = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(t);
  }, [intervalMs, active]);
  return now;
}

export const fmt = {
  bytes(n: number) {
    if (n < 1024) return `${n} B`;
    if (n < 1024 ** 2) return `${(n / 1024).toFixed(0)} KB`;
    return `${(n / 1024 ** 2).toFixed(1)} MB`;
  },
  count(n: number) {
    if (n >= 1e6) return `${(n / 1e6).toFixed(n >= 1e8 ? 0 : 1)}M`;
    if (n >= 1e3) return `${(n / 1e3).toFixed(0)}K`;
    return String(n);
  },
  duration(ms?: number) {
    if (ms == null) return "";
    if (ms < 1000) return `${ms} ms`;
    if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
    return `${Math.floor(ms / 60_000)}m ${Math.round((ms % 60_000) / 1000)}s`;
  },
  uptime(ms: number) {
    const s = Math.max(0, Math.floor(ms / 1000));
    const p = (n: number) => String(n).padStart(2, "0");
    return s >= 3600 ? `${Math.floor(s / 3600)}:${p(Math.floor((s % 3600) / 60))}:${p(s % 60)}` : `${p(Math.floor(s / 60))}:${p(s % 60)}`;
  },
  ago(t: number, now: number) {
    const s = Math.max(0, Math.round((now - t) / 1000));
    if (s < 10) return "now";
    if (s < 60) return `${s}s ago`;
    if (s < 3600) return `${Math.floor(s / 60)}m ago`;
    if (s < 86_400) return `${Math.floor(s / 3600)}h ago`;
    return `${Math.floor(s / 86_400)}d ago`;
  },
  clock(t: number) {
    return new Date(t).toLocaleTimeString([], { hour12: false });
  },
};

export async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
