import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { MARK_COLORS } from "./editor";

export interface MenuAction {
  label: string;
  icon: ReactNode;
  run: () => void;
  disabled?: boolean;
  danger?: boolean;
  shortcut?: string;
}
interface Props {
  x: number;
  y: number;
  title: string;
  actions: MenuAction[];
  canColor: boolean;
  onColor: (color: string | null) => void;
  onClose: () => void;
}

export default function ContextMenu({ x, y, title, actions, canColor, onColor, onClose }: Props) {
  const menu = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState({ left: x, top: y });
  useLayoutEffect(() => {
    const bounds = menu.current?.getBoundingClientRect();
    if (bounds) setPosition({ left: Math.max(8, Math.min(x, window.innerWidth - bounds.width - 8)),
      top: Math.max(8, Math.min(y, window.innerHeight - bounds.height - 8)) });
    menu.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
  }, [x, y]);
  useEffect(() => {
    const close = (event: PointerEvent) => { if (!menu.current?.contains(event.target as Node)) onClose(); };
    window.addEventListener("pointerdown", close);
    window.addEventListener("resize", onClose);
    return () => { window.removeEventListener("pointerdown", close); window.removeEventListener("resize", onClose); };
  }, [onClose]);
  return <div ref={menu} className="data-context-menu" role="menu" aria-label="数据操作" style={position}
    onContextMenu={event => event.preventDefault()} onKeyDown={event => {
      if (event.key === "Escape") { event.preventDefault(); onClose(); }
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const buttons = [...(menu.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
        const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
        buttons[(index + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length]?.focus();
      }
    }}>
    <div className="context-title">{title}</div>
    {actions.map(action => <button key={action.label} role="menuitem" aria-label={action.label} className={action.danger ? "context-danger" : ""}
      disabled={action.disabled} onClick={() => { onClose(); action.run(); }}>
      {action.icon}<span>{action.label}</span>{action.shortcut && <kbd>{action.shortcut}</kbd>}
    </button>)}
    <div className="context-colors" role="group" aria-label="单元格标记颜色">
      <span>标记颜色</span>
      <div>{MARK_COLORS.map(color => <button key={color.value} role="menuitem" aria-label={`标记为${color.name}`}
        title={color.name} style={{ background: color.value }} disabled={!canColor}
        onClick={() => { onClose(); onColor(color.value); }} />)}
        <button role="menuitem" className="clear-color" aria-label="清除颜色标记" title="清除颜色" disabled={!canColor}
          onClick={() => { onClose(); onColor(null); }}>×</button></div>
    </div>
  </div>;
}
