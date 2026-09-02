// contract: t8-fe-onboarding owns the implementation
// Stable surface consumed by t6-fe-shell: settings screen (re-runnable setup).
import OnboardingPanels from "../onboarding/OnboardingPanels";
import { IconButton } from "../../components/primitives";
import "./settings.css";

export interface SettingsMenuProps {
  onClose: () => void;
}

export default function SettingsMenu({ onClose }: SettingsMenuProps) {
  return (
    <section className="settings-menu onboarding-light" aria-label="설정">
      <div className="settings-menu__panel">
        <header className="settings-menu__header">
          <h2 className="settings-menu__title">설정</h2>
          <IconButton label="닫기" onClick={onClose}>
            ✕
          </IconButton>
        </header>
        <OnboardingPanels variant="settings" />
      </div>
    </section>
  );
}
