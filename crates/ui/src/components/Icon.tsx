export function Icon({ name, className }: { name: string; className?: string }) {
  const common = {
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 2,
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
    home: (
      <svg {...common}>
        <path d="M3 10.5 12 3l9 7.5" />
        <path d="M5 9.5V21h14V9.5" />
      </svg>
    ),
    folder: (
      <svg {...common}>
        <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a3 3 0 0 1-3 3H6a3 3 0 0 1-3-3V7z" />
      </svg>
    ),
    file: (
      <svg {...common}>
        <path d="M14 2H7a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V7z" />
        <path d="M14 2v5h5" />
      </svg>
    ),
    "file-text": (
      <svg {...common}>
        <path d="M14 2H7a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V7z" />
        <path d="M14 2v5h5" />
        <path d="M9 13h6" />
        <path d="M9 17h6" />
        <path d="M9 9h2" />
      </svg>
    ),
    "arrow-left": (
      <svg {...common}>
        <path d="m12 19-7-7 7-7" />
        <path d="M19 12H5" />
      </svg>
    ),
    "refresh-cw": (
      <svg {...common}>
        <path d="M21 12a9 9 0 1 1-2.64-6.36" />
        <path d="M21 3v6h-6" />
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
        <path d="M19.4 15a1 1 0 0 0 .2 1.1l.1.1a1 1 0 0 1 0 1.4l-1.2 1.2a1 1 0 0 1-1.4 0l-.1-.1a1 1 0 0 0-1.1-.2 1 1 0 0 0-.6.9V20a1 1 0 0 1-1 1h-1.7a1 1 0 0 1-1-1v-.2a1 1 0 0 0-.6-.9 1 1 0 0 0-1.1.2l-.1.1a1 1 0 0 1-1.4 0L4.6 18a1 1 0 0 1 0-1.4l.1-.1a1 1 0 0 0 .2-1.1 1 1 0 0 0-.9-.6H4a1 1 0 0 1-1-1v-1.7a1 1 0 0 1 1-1h.2a1 1 0 0 0 .9-.6 1 1 0 0 0-.2-1.1l-.1-.1a1 1 0 0 1 0-1.4L6 4.6a1 1 0 0 1 1.4 0l.1.1a1 1 0 0 0 1.1.2 1 1 0 0 0 .6-.9V4a1 1 0 0 1 1-1h1.7a1 1 0 0 1 1 1v.2a1 1 0 0 0 .6.9 1 1 0 0 0 1.1-.2l-.1-.1a1 1 0 0 1 1.4 0L19.4 6a1 1 0 0 1 0 1.4l-.1.1a1 1 0 0 0-.2 1.1 1 1 0 0 0 .9.6h.2a1 1 0 0 1 1 1v1.7a1 1 0 0 1-1 1H20a1 1 0 0 0-.6.6Z" />
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
    // New icons for jj.js compatibility
    bot: (
      <svg {...common}><rect x="3" y="11" width="18" height="10" rx="2" /><circle cx="12" cy="5" r="2" /><path d="M12 7v4" /><line x1="8" y1="16" x2="8" y2="16" /><line x1="16" y1="16" x2="16" y2="16" /></svg>
    ),
    zap: (
      <svg {...common}><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" /></svg>
    ),
    shield: (
      <svg {...common}><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" /></svg>
    ),
    automation: (
      <svg {...common}><polyline points="16 3 21 3 21 8" /><line x1="4" y1="20" x2="21" y2="3" /><polyline points="21 16 21 21 16 21" /><line x1="15" y1="15" x2="21" y2="21" /><line x1="4" y1="4" x2="9" y2="9" /></svg>
    ),
    dashboard: (
      <svg {...common}><rect x="3" y="3" width="7" height="9" rx="1" /><rect x="14" y="3" width="7" height="5" rx="1" /><rect x="14" y="12" width="7" height="9" rx="1" /><rect x="3" y="16" width="7" height="5" rx="1" /></svg>
    ),
    chat: (
      <svg {...common}><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" /></svg>
    ),
    skills: (
      <svg {...common}><path d="M12 2L2 7l10 5 10-5-10-5z" /><path d="M2 17l10 5 10-5" /><path d="M2 12l10 5 10-5" /></svg>
    ),
    logs: (
      <svg {...common}><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" /><polyline points="14 2 14 8 20 8" /><line x1="16" y1="13" x2="8" y2="13" /><line x1="16" y1="17" x2="8" y2="17" /></svg>
    ),
    plus: (
      <svg {...common}><line x1="12" y1="5" x2="12" y2="19" /><line x1="5" y1="12" x2="19" y2="12" /></svg>
    ),
    play: (
      <svg {...common}><polygon points="5 3 19 12 5 21 5 3" /></svg>
    ),
    clock: (
      <svg {...common}><circle cx="12" cy="12" r="10" /><polyline points="12 6 12 12 16 14" /></svg>
    ),
    eye: (
      <svg {...common}><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" /><circle cx="12" cy="12" r="3" /></svg>
    ),
    globe: (
      <svg {...common}><circle cx="12" cy="12" r="10" /><line x1="2" y1="12" x2="22" y2="12" /><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" /></svg>
    ),
    cpu: (
      <svg {...common}><rect x="4" y="4" width="16" height="16" rx="2" /><rect x="9" y="9" width="6" height="6" /><line x1="9" y1="1" x2="9" y2="4" /><line x1="15" y1="1" x2="15" y2="4" /><line x1="9" y1="20" x2="9" y2="23" /><line x1="15" y1="20" x2="15" y2="23" /><line x1="20" y1="9" x2="23" y2="9" /><line x1="20" y1="14" x2="23" y2="14" /><line x1="1" y1="9" x2="4" y2="9" /><line x1="1" y1="14" x2="4" y2="14" /></svg>
    ),
    check: (
      <svg {...common}><polyline points="20 6 9 17 4 12" /></svg>
    ),
    alert: (
      <svg {...common}><circle cx="12" cy="12" r="10" /><line x1="12" y1="8" x2="12" y2="12" /><line x1="12" y1="16" x2="12.01" y2="16" /></svg>
    ),
    terminal: (
      <svg {...common}><polyline points="4 17 10 11 4 5" /><line x1="12" y1="19" x2="20" y2="19" /></svg>
    ),
    send: (
      <svg {...common}><line x1="22" y1="2" x2="11" y2="13" /><polygon points="22 2 15 22 11 13 2 9 22 2" /></svg>
    ),
    upload: (
      <svg {...common}><path d="M12 16V4" /><path d="m7 9 5-5 5 5" /><path d="M20 16.5V18a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2v-1.5" /></svg>
    ),
    activity: (
      <svg {...common}><polyline points="22 12 18 12 15 21 9 3 6 12 2 12" /></svg>
    ),
    network: (
      <svg {...common}><rect x="9" y="2" width="6" height="6" rx="1" /><rect x="2" y="16" width="6" height="6" rx="1" /><rect x="16" y="16" width="6" height="6" rx="1" /><path d="M12 8v4M6 16l6-4 6 4" /></svg>
    ),
    refresh: (
      <svg {...common}><polyline points="23 4 23 10 17 10" /><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" /></svg>
    ),
    trash: (
      <svg {...common}><polyline points="3 6 5 6 21 6" /><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" /></svg>
    ),
    layers: (
      <svg {...common}><polygon points="12 2 2 7 12 12 22 7 12 2" /><polyline points="2 17 12 22 22 17" /><polyline points="2 12 12 17 22 12" /></svg>
    ),
    link: (
      <svg {...common}><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" /><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" /></svg>
    ),
    paperclip: (
      <svg {...common}><path d="m21.44 11.05-8.49 8.49a6 6 0 0 1-8.49-8.49l9.19-9.2a4 4 0 0 1 5.66 5.66l-9.2 9.2a2 2 0 0 1-2.82-2.83l8.49-8.48" /></svg>
    ),
    pause: (
      <svg {...common}><rect x="6" y="4" width="4" height="16" /><rect x="14" y="4" width="4" height="16" /></svg>
    ),
    archive: (
      <svg {...common}><polyline points="21 8 21 21 3 21 3 8" /><rect x="1" y="3" width="22" height="5" /><line x1="10" y1="12" x2="14" y2="12" /></svg>
    ),
    users: (
      <svg {...common}><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" /><circle cx="9" cy="7" r="4" /><path d="M23 21v-2a4 4 0 0 0-3-3.87" /><path d="M16 3.13a4 4 0 0 1 0 7.75" /></svg>
    ),
    "bar-chart": (
      <svg {...common}><line x1="12" y1="20" x2="12" y2="10" /><line x1="18" y1="20" x2="18" y2="4" /><line x1="6" y1="20" x2="6" y2="16" /></svg>
    ),
    "scroll-text": (
      <svg {...common}><path d="M8 21h12a2 2 0 0 0 2-2v-2H10v2a2 2 0 1 1-4 0V5a2 2 0 1 0-4 0v3h4" /><path d="M19 17V5a2 2 0 0 0-2-2H4" /><path d="M15 8h-5" /><path d="M15 12h-5" /></svg>
    ),
    puzzle: (
      <svg {...common}><path d="M19.439 7.85c-.049.322.059.648.289.878l1.568 1.568c.47.47.706 1.087.706 1.704s-.235 1.233-.706 1.704l-1.611 1.611a.98.98 0 0 1-.837.276c-.47-.07-.802-.48-.968-.925a2.501 2.501 0 1 0-3.214 3.214c.446.166.855.497.925.968a.979.979 0 0 1-.276.837l-1.61 1.61a2.404 2.404 0 0 1-1.705.707 2.402 2.402 0 0 1-1.704-.706l-1.568-1.568a1.026 1.026 0 0 0-.877-.29c-.493.074-.84.504-1.02.968a2.5 2.5 0 1 1-3.237-3.237c.464-.18.894-.527.967-1.02a1.026 1.026 0 0 0-.289-.877l-1.568-1.568A2.402 2.402 0 0 1 1.998 12c0-.617.236-1.234.706-1.704L4.23 8.77c.24-.24.581-.353.917-.303.515.077.877.528 1.073 1.01a2.5 2.5 0 1 0 3.259-3.259c-.482-.196-.933-.558-1.01-1.073-.05-.336.062-.676.303-.917l1.525-1.525A2.402 2.402 0 0 1 12 2c.617 0 1.234.236 1.704.706l1.568 1.568c.23.23.556.338.878.29.493-.074.84-.504 1.02-.968a2.5 2.5 0 1 1 3.237 3.237c-.464.18-.894.527-.968 1.02Z" /></svg>
    ),
    plug: (
      <svg {...common}><path d="M12 22v-5" /><path d="M9 8V2" /><path d="M15 8V2" /><path d="M18 8v5a6 6 0 0 1-6 6v0a6 6 0 0 1-6-6V8Z" /></svg>
    ),
    mic: (
      <svg {...common}><path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" /><path d="M19 10v2a7 7 0 0 1-14 0v-2" /><line x1="12" y1="19" x2="12" y2="23" /><line x1="8" y1="23" x2="16" y2="23" /></svg>
    ),
    "mic-off": (
      <svg {...common}><line x1="1" y1="1" x2="23" y2="23" /><path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" /><path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" /><line x1="12" y1="19" x2="12" y2="23" /><line x1="8" y1="23" x2="16" y2="23" /></svg>
    ),
    loader: (
      <svg {...common}><line x1="12" y1="2" x2="12" y2="6" /><line x1="12" y1="18" x2="12" y2="22" /><line x1="4.93" y1="4.93" x2="7.76" y2="7.76" /><line x1="16.24" y1="16.24" x2="19.07" y2="19.07" /><line x1="2" y1="12" x2="6" y2="12" /><line x1="18" y1="12" x2="22" y2="12" /><line x1="4.93" y1="19.07" x2="7.76" y2="16.24" /><line x1="16.24" y1="7.76" x2="19.07" y2="4.93" /></svg>
    ),
    "chevron-down": (
      <svg {...common}><polyline points="6 9 12 15 18 9" /></svg>
    ),
  };

  return <span className={`icon-glyph ${className ?? ""}`} aria-hidden>{icons[name] ?? icons.search}</span>;
}
