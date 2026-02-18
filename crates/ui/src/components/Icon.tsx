export function Icon({ name }: { name: string }) {
  const common = {
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.9,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };

  const icons: Record<string, JSX.Element> = {
    brand: (
      <svg {...common}>
        <path d="M12 3v3" />
        <rect x="6" y="8" width="12" height="10" rx="3" />
        <circle cx="10" cy="13" r="1" />
        <circle cx="14" cy="13" r="1" />
        <path d="M9 18v2M15 18v2" />
      </svg>
    ),
    now: (
      <svg {...common}>
        <path d="M4 10.5L12 4l8 6.5" />
        <path d="M6 10v9h12v-9" />
      </svg>
    ),
    ask: (
      <svg {...common}>
        <path d="M20 12a8 8 0 1 1-3.2-6.4A8 8 0 0 1 20 12Z" />
        <path d="M8.5 14.5l-1.5 4 4-1.5" />
      </svg>
    ),
    routines: (
      <svg {...common}>
        <path d="M17 2v4h-4" />
        <path d="M7 22v-4h4" />
        <path d="M20 11a8 8 0 0 0-13.66-5.66L3 9" />
        <path d="M4 13a8 8 0 0 0 13.66 5.66L21 15" />
      </svg>
    ),
    accounts: (
      <svg {...common}>
        <circle cx="12" cy="8" r="3.5" />
        <path d="M4.5 20a7.5 7.5 0 0 1 15 0" />
      </svg>
    ),
    library: (
      <svg {...common}>
        <path d="M4 6.5A2.5 2.5 0 0 1 6.5 4H20v14H6.5A2.5 2.5 0 0 0 4 20Z" />
        <path d="M8 7h8M8 11h8M8 15h6" />
      </svg>
    ),
    search: (
      <svg {...common}>
        <circle cx="11" cy="11" r="6.5" />
        <path d="m20 20-4-4" />
      </svg>
    ),
    bell: (
      <svg {...common}>
        <path d="M15 17H5l1.5-2v-4A5.5 5.5 0 0 1 12 5.5V5a2 2 0 1 1 4 0v.5A5.5 5.5 0 0 1 21.5 11v4L23 17h-8" />
        <path d="M14.5 20a2.5 2.5 0 0 1-5 0" />
      </svg>
    ),
    "safe-on": (
      <svg {...common}>
        <path d="M12 3.5 5.5 6v5.6c0 4.1 2.8 7.9 6.5 8.9 3.7-1 6.5-4.8 6.5-8.9V6L12 3.5Z" />
        <path d="m9.2 12.1 1.9 1.9 3.7-3.8" />
      </svg>
    ),
    "safe-off": (
      <svg {...common}>
        <path d="M12 3.5 5.5 6v5.6c0 4.1 2.8 7.9 6.5 8.9 3.7-1 6.5-4.8 6.5-8.9V6L12 3.5Z" />
        <path d="m9.2 9.2 5.6 5.6" />
        <path d="m14.8 9.2-5.6 5.6" />
      </svg>
    ),
    settings: (
      <svg {...common}>
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1 1 0 0 0 .2 1.1l.1.1a1 1 0 0 1 0 1.4l-1.2 1.2a1 1 0 0 1-1.4 0l-.1-.1a1 1 0 0 0-1.1-.2 1 1 0 0 0-.6.9V20a1 1 0 0 1-1 1h-1.7a1 1 0 0 1-1-1v-.2a1 1 0 0 0-.6-.9 1 1 0 0 0-1.1.2l-.1.1a1 1 0 0 1-1.4 0L4.6 18a1 1 0 0 1 0-1.4l.1-.1a1 1 0 0 0 .2-1.1 1 1 0 0 0-.9-.6H4a1 1 0 0 1-1-1v-1.7a1 1 0 0 1 1-1h.2a1 1 0 0 0 .9-.6 1 1 0 0 0-.2-1.1l-.1-.1a1 1 0 0 1 0-1.4L6 4.6a1 1 0 0 1 1.4 0l.1.1a1 1 0 0 0 1.1.2 1 1 0 0 0 .6-.9V4a1 1 0 0 1 1-1h1.7a1 1 0 0 1 1 1v.2a1 1 0 0 0 .6.9 1 1 0 0 0 1.1-.2l.1-.1a1 1 0 0 1 1.4 0L19.4 6a1 1 0 0 1 0 1.4l-.1.1a1 1 0 0 0-.2 1.1 1 1 0 0 0 .9.6h.2a1 1 0 0 1 1 1v1.7a1 1 0 0 1-1 1H20a1 1 0 0 0-.6.6Z" />
      </svg>
    ),
    "collapse-left": (
      <svg {...common}>
        <path d="M14.5 6 8.5 12l6 6" />
      </svg>
    ),
    "collapse-right": (
      <svg {...common}>
        <path d="M9.5 6 15.5 12l-6 6" />
      </svg>
    ),
    close: (
      <svg {...common}>
        <path d="m7 7 10 10" />
        <path d="m17 7-10 10" />
      </svg>
    ),
  };

  return <span className="icon-glyph" aria-hidden>{icons[name] ?? icons.search}</span>;
}
