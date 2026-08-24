import type { SVGProps } from "react";

export type IconName =
  | "activity"
  | "alert"
  | "arrow-up-right"
  | "check"
  | "chevron-down"
  | "chevron-right"
  | "clipboard"
  | "close"
  | "code"
  | "copy"
  | "file"
  | "folder"
  | "folder-open"
  | "keyboard"
  | "link"
  | "message"
  | "moon"
  | "more"
  | "play"
  | "plug"
  | "plus"
  | "refresh"
  | "search"
  | "send"
  | "settings"
  | "shield"
  | "spark"
  | "star"
  | "stop"
  | "sun"
  | "terminal"
  | "trash"
  | "upload"
  | "x";

interface Props extends SVGProps<SVGSVGElement> {
  name: IconName;
  size?: number;
  strokeWidth?: number;
}

/**
 * Small, dependency-free Lucide-style icon set.
 * Structural icons stay vector-based so the UI remains crisp at Windows DPI scales.
 */
export function Icon({ name, size = 16, strokeWidth = 1.8, ...props }: Props) {
  const common = {
    fill: "none",
    stroke: "currentColor",
    strokeWidth,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };

  const paths: Record<IconName, React.ReactNode> = {
    activity: <><path d="M3 12h3l2-7 4 14 2-7h4" /></>,
    alert: <><path d="M10.6 3.4 2.8 17a1 1 0 0 0 .87 1.5h12.66a1 1 0 0 0 .87-1.5L9.4 3.4a1 1 0 0 0-1.74 0Z" /><path d="M8 8.5v4M8 15.5h.01" /></>,
    "arrow-up-right": <><path d="M5 19 19 5M9 5h10v10" /></>,
    check: <path d="m4 12 5 5L20 6" />,
    "chevron-down": <path d="m6 9 6 6 6-6" />,
    "chevron-right": <path d="m9 6 6 6-6 6" />,
    clipboard: <><rect x="6" y="4" width="12" height="16" rx="2" /><path d="M9 4.5V3h6v1.5M9 10h6M9 14h4" /></>,
    close: <><path d="m6 6 12 12M18 6 6 18" /></>,
    code: <><path d="m8 8-4 4 4 4M16 8l4 4-4 4M14 4l-4 16" /></>,
    copy: <><rect x="8" y="8" width="11" height="11" rx="2" /><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" /></>,
    file: <><path d="M6 3h7l5 5v13H6z" /><path d="M13 3v5h5M9 13h6M9 17h4" /></>,
    folder: <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4l2 2h5A1.5 1.5 0 0 1 17 8.5v7A1.5 1.5 0 0 1 15.5 17h-11A1.5 1.5 0 0 1 3 15.5z" />,
    "folder-open": <><path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4l2 2h6A1.5 1.5 0 0 1 18 9.5" /><path d="m3 10 1.2 6.1A1.5 1.5 0 0 0 5.7 17h9.8a1.5 1.5 0 0 0 1.45-1.1L18.5 10z" /></>,
    keyboard: <><rect x="3" y="6" width="18" height="12" rx="2" /><path d="M6 10h.01M9 10h.01M12 10h.01M15 10h.01M18 10h.01M6 14h8M16 14h2" /></>,
    link: <><path d="M10 13a5 5 0 0 0 7.07.07l1.42-1.42a5 5 0 0 0-7.07-7.07L10.6 5.4" /><path d="M14 11a5 5 0 0 0-7.07-.07L5.5 12.35a5 5 0 0 0 7.07 7.07l.82-.82" /></>,
    message: <><path d="M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v7a2.5 2.5 0 0 1-2.5 2.5H10l-4.5 4v-4.1A2.5 2.5 0 0 1 4 12.5z" /><path d="M8 8h8M8 11h5" /></>,
    moon: <path d="M19.5 14.7A7.5 7.5 0 0 1 9.3 4.5 7.5 7.5 0 1 0 19.5 14.7Z" />,
    more: <><circle cx="5" cy="12" r=".8" fill="currentColor" stroke="none" /><circle cx="12" cy="12" r=".8" fill="currentColor" stroke="none" /><circle cx="19" cy="12" r=".8" fill="currentColor" stroke="none" /></>,
    play: <path d="m8 5 11 7-11 7z" />,
    plug: <><path d="M9 7V3M15 7V3M7 7h10v3a5 5 0 0 1-10 0zM12 15v6" /></>,
    plus: <><path d="M12 5v14M5 12h14" /></>,
    refresh: <><path d="M20 11a8 8 0 0 0-14.8-4L3 10" /><path d="M3 5v5h5M4 13a8 8 0 0 0 14.8 4L21 14" /><path d="M21 19v-5h-5" /></>,
    search: <><circle cx="10.8" cy="10.8" r="6.5" /><path d="m16 16 4.5 4.5" /></>,
    send: <><path d="m21 3-7.2 18-3.2-7.6L3 10.2z" /><path d="M10.6 13.4 21 3" /></>,
    settings: <><path d="M12 8.2a3.8 3.8 0 1 0 0 7.6 3.8 3.8 0 0 0 0-7.6Z" /><path d="m19.4 13.5 1.3 1-.9 1.6-1.6-.5a7.7 7.7 0 0 1-1.8 1.1l-.2 1.7h-1.9l-.5-1.6a8 8 0 0 1-2.2 0l-.5 1.6H9.2L9 16.7a7.7 7.7 0 0 1-1.8-1.1l-1.6.5-.9-1.6 1.3-1a7.7 7.7 0 0 1 0-3l-1.3-1 .9-1.6 1.6.5A7.7 7.7 0 0 1 9 7.3l.2-1.7h1.9l.5 1.6a8 8 0 0 1 2.2 0l.5-1.6h1.9l.2 1.7a7.7 7.7 0 0 1 1.8 1.1l1.6-.5.9 1.6-1.3 1a7.7 7.7 0 0 1 0 3Z" /></>,
    shield: <><path d="M12 3 19 6v5c0 4.3-2.8 7.8-7 10-4.2-2.2-7-5.7-7-10V6z" /><path d="m9 12 2 2 4-4" /></>,
    spark: <><path d="m12 3 1.2 5.1L18 10l-4.8 1.9L12 17l-1.2-5.1L6 10l4.8-1.9z" /><path d="m18.5 15 .5 2 2 .5-2 .5-.5 2-.5-2-2-.5 2-.5z" /></>,
    star: <path d="m12 3 2.8 5.7 6.2.9-4.5 4.4 1.1 6.2-5.6-3-5.6 3 1.1-6.2L3 9.6l6.2-.9z" />,
    stop: <rect x="6" y="6" width="12" height="12" rx="1.5" />,
    sun: <><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></>,
    terminal: <><path d="m4 5 7 7-7 7" /><path d="M13 19h7" /></>,
    trash: <><path d="M4 7h16M10 11v5M14 11v5M6 7l1 13h10l1-13M9 7V4h6v3" /></>,
    upload: <><path d="M12 16V4M8 8l4-4 4 4M5 20h14" /></>,
    x: <><path d="m6 6 12 12M18 6 6 18" /></>,
  };

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
      {...common}
      {...props}
    >
      {paths[name]}
    </svg>
  );
}

export function IconButton({
  label,
  children,
  className = "",
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return (
    <button
      type="button"
      className={`icon-button ${className}`}
      aria-label={label}
      title={label}
      {...props}
    >
      {children}
    </button>
  );
}
