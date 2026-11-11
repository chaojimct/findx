import type { ReactNode } from "react";

type IconProps = { className?: string };

function Svg({ className, children }: IconProps & { children: ReactNode }) {
  return (
    <svg
      className={className}
      width="16"
      height="16"
      viewBox="0 0 16 16"
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

const stroke = {
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.25,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
};

export function IconHistory({ className }: IconProps) {
  return (
    <Svg className={className}>
      <circle cx="8" cy="8" r="5.25" {...stroke} />
      <path d="M8 5.2V8.2l2.2 1.3" {...stroke} />
    </Svg>
  );
}

export function IconSidebar({ className }: IconProps) {
  return (
    <Svg className={className}>
      <rect x="2.25" y="2.75" width="11.5" height="10.5" rx="1.2" {...stroke} />
      <path d="M6.2 3.1v9.8" {...stroke} />
    </Svg>
  );
}

export function IconPreview({ className }: IconProps) {
  return (
    <Svg className={className}>
      <path
        d="M2.2 8s2.2-3.4 5.8-3.4S13.8 8 13.8 8s-2.2 3.4-5.8 3.4S2.2 8 2.2 8z"
        {...stroke}
      />
      <circle cx="8" cy="8" r="1.6" {...stroke} />
    </Svg>
  );
}

export function IconSettings({ className }: IconProps) {
  return (
    <Svg className={className}>
      <circle cx="8" cy="8" r="2" {...stroke} />
      <path
        d="M8 2.4v1.3M8 12.3v1.3M2.4 8h1.3M12.3 8h1.3M4.1 4.1l.9.9M11 11l.9.9M11.9 4.1 11 5M5 11l-.9.9"
        {...stroke}
      />
    </Svg>
  );
}
