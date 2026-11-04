/** 侧边栏 / 顶部栏用到的线框图标，统一 20×20、1.7 描边。 */
const S = {
  viewBox: "0 0 20 20",
  width: 18,
  height: 18,
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.7,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
};

export const IconOverview = () => (
  <svg {...S}>
    <rect x="3" y="3" width="6" height="6" rx="1.5" />
    <rect x="11" y="3" width="6" height="6" rx="1.5" />
    <rect x="3" y="11" width="6" height="6" rx="1.5" />
    <rect x="11" y="11" width="6" height="6" rx="1.5" />
  </svg>
);

export const IconPositions = () => (
  <svg {...S}>
    <path d="M3 15l4-5 3 3 7-8" />
    <path d="M13 5h4v4" />
  </svg>
);

export const IconTrades = () => (
  <svg {...S}>
    <path d="M4 5h12M4 10h12M4 15h8" />
  </svg>
);

export const IconStrategy = () => (
  <svg {...S}>
    <circle cx="6" cy="6" r="2.2" />
    <circle cx="14" cy="14" r="2.2" />
    <path d="M8.2 6H17M3 14h8.8" />
  </svg>
);

export const IconAi = () => (
  <svg {...S}>
    <rect x="4" y="6" width="12" height="10" rx="2.5" />
    <path d="M10 3v3M7.5 11h.01M12.5 11h.01M2 10v3M18 10v3" />
  </svg>
);

export const IconAccount = () => (
  <svg {...S}>
    <rect x="3" y="5" width="14" height="11" rx="2" />
    <path d="M3 9h14M7 13h2" />
  </svg>
);

export const IconNotify = () => (
  <svg {...S}>
    <path d="M17 3L3 9l5 2 2 5z" />
    <path d="M8 11l9-8" />
  </svg>
);

export const IconLogs = () => (
  <svg {...S}>
    <rect x="3" y="3" width="14" height="14" rx="2" />
    <path d="M6.5 8l2 2-2 2M10.5 12h3" />
  </svg>
);

export const IconModel = () => (
  <svg {...S}>
    <rect x="5" y="5" width="10" height="10" rx="2" />
    <path d="M8 2v3M12 2v3M8 15v3M12 15v3M2 8h3M2 12h3M15 8h3M15 12h3" />
  </svg>
);

export const IconKnowledge = () => (
  <svg {...S}>
    <path d="M4 4.5A1.5 1.5 0 0 1 5.5 3H16v13H5.5A1.5 1.5 0 0 0 4 17.5z" />
    <path d="M4 17.5A1.5 1.5 0 0 0 5.5 19H16M8 7h4.5M8 10h4.5" />
  </svg>
);

export const IconMenu = () => (
  <svg {...S}>
    <path d="M3 6h14M3 10h14M3 14h14" />
  </svg>
);

export const IconMore = () => (
  <svg {...S}>
    <circle cx="4.5" cy="10" r="1.2" fill="currentColor" stroke="none" />
    <circle cx="10" cy="10" r="1.2" fill="currentColor" stroke="none" />
    <circle cx="15.5" cy="10" r="1.2" fill="currentColor" stroke="none" />
  </svg>
);

export const IconRefresh = () => (
  <svg {...S}>
    <path d="M16.5 10a6.5 6.5 0 1 1-2-4.7" />
    <path d="M16.8 3.2v3.6h-3.6" />
  </svg>
);
