// contract: t8-fe-onboarding owns the implementation
// Stable surface consumed by t6-fe-shell: full-screen first-run onboarding.
import OnboardingPanels from "./OnboardingPanels";
import "./onboarding.css";

export interface OnboardingFlowProps {
  /** Called when onboarding completes; t6 sets the localStorage flag routing. */
  onComplete: () => void;
}

export default function OnboardingFlow({ onComplete }: OnboardingFlowProps) {
  return (
    <section className="onboarding-flow onboarding-light" aria-label="온보딩">
      <div className="onboarding-flow__card">
        <OnboardingPanels variant="onboarding" onComplete={onComplete} />
      </div>
    </section>
  );
}
