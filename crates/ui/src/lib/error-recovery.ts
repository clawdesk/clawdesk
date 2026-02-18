export type ErrorCategory = "network" | "auth" | "security" | "rate_limit" | "unknown";

export interface RecoveryAction {
  label: string;
}

export interface RecoverableError {
  category: ErrorCategory;
  userMessage: string;
  technicalDetail: string;
  actions: RecoveryAction[];
}

function stringifyError(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return String(error);
}

export function classifyError(error: unknown): RecoverableError {
  const detail = stringifyError(error);
  const lowered = detail.toLowerCase();

  if (lowered.includes("network") || lowered.includes("connect") || lowered.includes("unreachable")) {
    return {
      category: "network",
      userMessage: "ClawDesk could not reach the local engine.",
      technicalDetail: detail,
      actions: [{ label: "Run Quick Check" }, { label: "Restart local engine" }],
    };
  }

  if (lowered.includes("api key") || lowered.includes("unauthorized") || lowered.includes("forbidden")) {
    return {
      category: "auth",
      userMessage: "Your provider key appears invalid or expired.",
      technicalDetail: detail,
      actions: [{ label: "Update API key" }, { label: "Switch provider" }],
    };
  }

  if (lowered.includes("scan") || lowered.includes("blocked") || lowered.includes("policy")) {
    return {
      category: "security",
      userMessage: "Safety checks blocked this action.",
      technicalDetail: detail,
      actions: [{ label: "Review flagged content" }, { label: "Adjust request and retry" }],
    };
  }

  if (lowered.includes("rate") || lowered.includes("429") || lowered.includes("throttle")) {
    return {
      category: "rate_limit",
      userMessage: "Provider rate limit was reached. Try again in a moment.",
      technicalDetail: detail,
      actions: [{ label: "Retry" }, { label: "Use a lighter model" }],
    };
  }

  return {
    category: "unknown",
    userMessage: "Something went wrong. You can retry safely.",
    technicalDetail: detail,
    actions: [{ label: "Retry" }, { label: "Open diagnostics" }],
  };
}

