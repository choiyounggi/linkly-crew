// contract: t8-fe-onboarding owns the implementation
// Stable surface consumed by t6-fe-shell: full-screen first-run onboarding.
export interface OnboardingFlowProps {
  /** Called when onboarding completes; t6 sets the localStorage flag routing. */
  onComplete: () => void;
}

export default function OnboardingFlow(_props: OnboardingFlowProps) {
  return <section className="onboarding-flow" aria-label="온보딩" />;
}
