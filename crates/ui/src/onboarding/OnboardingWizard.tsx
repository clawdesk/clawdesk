import { useMemo, useState } from "react";
import type { HealthResponse } from "../types";
import { AGENT_TEMPLATES } from "../types";
import { Modal } from "../components/Modal";

export interface OnboardingResult {
  provider: string;
  apiKey: string;
  templateName: string;
  firstPrompt: string;
}

export function OnboardingWizard({
  open,
  health,
  onComplete,
}: {
  open: boolean;
  health: HealthResponse | null;
  onComplete: (result: OnboardingResult) => void;
}) {
  const [step, setStep] = useState(1);
  const [provider, setProvider] = useState("Anthropic");
  const [apiKey, setApiKey] = useState("");
  const [templateName, setTemplateName] = useState(AGENT_TEMPLATES[0].name);
  const [firstPrompt, setFirstPrompt] = useState("Draft a short team update from today's progress.");

  const selectedTemplate = useMemo(
    () => AGENT_TEMPLATES.find((template) => template.name === templateName),
    [templateName]
  );
  const localProvider = provider.toLowerCase().includes("ollama");

  if (!open) return null;

  return (
    <Modal title="Welcome to ClawDesk" onClose={() => undefined}>
      <div className="modal-stack onboarding">
        <div className="wizard-steps onboarding-steps">
          <span className={step === 1 ? "active" : ""}>1. Welcome</span>
          <span className={step === 2 ? "active" : ""}>2. API Key</span>
          <span className={step === 3 ? "active" : ""}>3. Assistant</span>
          <span className={step === 4 ? "active" : ""}>4. First Ask</span>
        </div>

        {step === 1 && (
          <section className="section-card">
            <div className="onboarding-brand">
              <img src="/logo.svg" alt="ClawDesk logo" className="onboarding-logo" />
              <div>
                <h3>Your local personal assistant.</h3>
                <p>Request, plan, approvals, and proof, all on your device.</p>
              </div>
            </div>
            <div className="list-rows">
              <div className="row-card">
                <div>
                  <div className="row-title">Engine status</div>
                  <div className="row-sub">
                    {health
                      ? `Connected (v${health.version})`
                      : "Checking backend availability..."}
                  </div>
                </div>
              </div>
            </div>
          </section>
        )}

        {step === 2 && (
          <section className="section-card">
            <label className="field-label">
              Provider
              <select value={provider} onChange={(event) => setProvider(event.target.value)}>
                <option>Anthropic</option>
                <option>OpenAI</option>
                <option>Google</option>
                <option>Ollama (Local)</option>
              </select>
            </label>
            <label className="field-label">
              API key
              <input
                type="password"
                value={apiKey}
                placeholder={localProvider ? "Not required for local Ollama" : "Paste your API key"}
                onChange={(event) => setApiKey(event.target.value)}
                disabled={localProvider}
              />
            </label>
            {localProvider ? (
              <p>Local mode selected. Make sure Ollama is running on this machine.</p>
            ) : (
              <p>Stored locally on this device. You can change this later in Settings.</p>
            )}
          </section>
        )}

        {step === 3 && (
          <section className="section-card">
            <p>Choose your default assistant style.</p>
            <div className="template-grid">
              {AGENT_TEMPLATES.map((template) => (
                <button
                  key={template.name}
                  className={`template-tile ${templateName === template.name ? "selected" : ""}`}
                  onClick={() => setTemplateName(template.name)}
                >
                  <div className="row-title">{template.icon} {template.name}</div>
                  <div className="row-sub">{template.description}</div>
                </button>
              ))}
            </div>
          </section>
        )}

        {step === 4 && (
          <section className="section-card">
            <div className="row-title">{selectedTemplate?.icon} {selectedTemplate?.name}</div>
            <p>Start with a ready-to-run request:</p>
            <label className="field-label">
              First request
              <textarea
                rows={4}
                value={firstPrompt}
                onChange={(event) => setFirstPrompt(event.target.value)}
              />
            </label>
          </section>
        )}

        <div className="row-actions onboarding-actions">
          <button
            className="btn ghost"
            disabled={step === 1}
            onClick={() => setStep((value) => Math.max(1, value - 1))}
          >
            Back
          </button>
          {step < 4 ? (
            <button
              className="btn primary"
              onClick={() => setStep((value) => Math.min(4, value + 1))}
              disabled={step === 2 && !localProvider && !apiKey.trim()}
            >
              Continue
            </button>
          ) : (
            <button
              className="btn primary"
              onClick={() =>
                onComplete({
                  provider,
                  apiKey,
                  templateName,
                  firstPrompt,
                })
              }
            >
              Start using ClawDesk
            </button>
          )}
        </div>
      </div>
    </Modal>
  );
}
