// contract: t8-fe-onboarding owns the implementation
// Stable surface consumed by t6-fe-shell: settings screen (re-runnable setup).
export interface SettingsMenuProps {
  onClose: () => void;
}

export default function SettingsMenu(_props: SettingsMenuProps) {
  return <section className="settings-menu" aria-label="설정" />;
}
