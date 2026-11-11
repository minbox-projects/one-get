import { Button } from "./components/ui/Button";
import { Dialog } from "./components/ui/Dialog";
import { Input } from "./components/ui/Input";

export default function App() {
  return (
    <main className="app-shell">
      <h1>OneGet</h1>
      <p>Desktop download manager shell baseline.</p>
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
