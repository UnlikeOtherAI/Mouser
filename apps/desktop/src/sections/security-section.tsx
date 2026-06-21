import { useState } from "react";
import { SectionCard } from "../components/section-card";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";

/** Trust & permission preferences (static placeholder). */
export function SecuritySection(): React.JSX.Element {
  const [requireApproval, setRequireApproval] = useState(true);
  const [encryptOnly, setEncryptOnly] = useState(true);
  const [lockOnSleep, setLockOnSleep] = useState(true);

  return (
    <div className="space-y-6">
      <SectionCard
        title="Trust"
        description="New devices must be approved before they can send input."
      >
        <SettingRow
          title="Require approval for new devices"
          description="Show a pairing prompt with a short verification code (SAS)."
          control={
            <Toggle
              label="Require approval for new devices"
              labelHidden
              checked={requireApproval}
              onChange={setRequireApproval}
            />
          }
        />
        <SettingRow
          title="Encrypted connections only"
          description="Refuse to connect to peers that fail certificate pinning."
          control={
            <Toggle
              label="Encrypted connections only"
              labelHidden
              checked={encryptOnly}
              onChange={setEncryptOnly}
            />
          }
        />
      </SectionCard>

      <SectionCard title="Session">
        <SettingRow
          title="Release input when locked"
          description="Return keyboard and mouse ownership to the local device on sleep or lock."
          control={
            <Toggle
              label="Release input when locked"
              labelHidden
              checked={lockOnSleep}
              onChange={setLockOnSleep}
            />
          }
        />
      </SectionCard>
    </div>
  );
}
