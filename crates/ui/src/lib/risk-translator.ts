export type RiskLevel = "low" | "medium" | "high";

export interface PlanTouch {
  account: string;
  type: string;
}

interface RiskTranslation {
  summary: string;
  consequence: string;
  undo: string;
}

const BASE_TRANSLATIONS: Record<RiskLevel, RiskTranslation> = {
  low: {
    summary: "Read-only preview flow",
    consequence: "No messages are sent and no files are changed.",
    undo: "Nothing to undo.",
  },
  medium: {
    summary: "Drafts or local files may change",
    consequence: "This may update drafts or write local output.",
    undo: "You can delete or edit the draft afterwards.",
  },
  high: {
    summary: "Real-world action",
    consequence: "This can send/write/execute and may be visible to others.",
    undo: "Some actions cannot be fully undone.",
  },
};

export function translateRisk(risk: RiskLevel, touches: PlanTouch[]): RiskTranslation {
  const base = BASE_TRANSLATIONS[risk];
  if (!touches.length) return base;
  const touchLabel = touches.map((touch) => `${touch.account} (${touch.type})`).join(", ");
  return {
    ...base,
    consequence: `${base.consequence} Affected: ${touchLabel}.`,
  };
}

