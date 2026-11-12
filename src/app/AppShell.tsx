import { useEffect, useState } from "react";

import { Button } from "../components/ui/Button";
import { Dialog } from "../components/ui/Dialog";
import { Input } from "../components/ui/Input";
import { DiagnosticsPanel } from "../features/diagnostics/DiagnosticsPanel";
import { invokeCommand } from "../lib/tauri";
import type { AppInfo } from "../types/app";

export function AppShell() {
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);

  useEffect(() => {
    let active = true;

    void invokeCommand<AppInfo>("app_info").then((result) => {
      if (active && result.ok) {
        setAppInfo(result.data);
      }
    });

    return () => {
      active = false;
    };
  }, []);

  const productName = appInfo?.name.trim() || "OneGet";

  return (
    <main className="app-shell">
      <header className="app-shell__header">
        <div className="app-shell__identity">
          <h1>{productName}</h1>
          {appInfo?.version ? (
            <p className="app-shell__version">Version {appInfo.version}</p>
          ) : null}
        </div>
        <p className="app-shell__tagline">
          Desktop download manager shell baseline.
        </p>
      </header>

      <DiagnosticsPanel />

      <section className="design-system" aria-labelledby="design-system-heading">
        <h2 id="design-system-heading">Design system</h2>
        <Input label="Preview name" name="preview-name" placeholder="OneGet" />
        <div className="design-system__actions">
          <Button>Preview button</Button>
          <Dialog
            trigger={<Button variant="secondary">Open dialog</Button>}
            title="Design system dialog"
            description="Preview of the shared dialog primitive."
          >
            <p>
              The dialog traps focus, closes on Escape, and restores focus to its
              trigger.
            </p>
          </Dialog>
        </div>
      </section>
    </main>
  );
}
