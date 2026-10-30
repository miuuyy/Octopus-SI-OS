import { clientExperiments, useClientExperimentState, useSetClientExperiment } from "../../clientExperiments";
import { OcCard, OcSettingsRow, OcSwitch } from "../../design-system/components";
import type { OctopusClientAccessProfile } from "../../electron";
import { settingsTargetProps } from "./settingsCatalog";
import { SettingsHeading } from "./SettingsHeading";

export function ExperimentsSection({ accessProfile }: { accessProfile: OctopusClientAccessProfile }) {
  const state = useClientExperimentState(accessProfile);
  const setExperiment = useSetClientExperiment(accessProfile);
  return (
    <section {...settingsTargetProps("experiments.list")} className="settings-section">
      <SettingsHeading
        description="Small product ideas running in this client. Their counts stay on this device, and you can turn any of them off."
        title="Experiments"
      />
      <OcCard>
        {clientExperiments.map((experiment) => (
          <OcSettingsRow
            control={(
              <OcSwitch
                checked={!state.disabled.includes(experiment.id)}
                label={experiment.title}
                onCheckedChange={(enabled) => setExperiment(experiment.id, enabled)}
              />
            )}
            description={`${experiment.description} ${experiment.question} ${state.signals[experiment.id] ?? 0} ${experiment.signalLabel} so far.`}
            key={experiment.id}
            label={experiment.title}
          />
        ))}
      </OcCard>
    </section>
  );
}
