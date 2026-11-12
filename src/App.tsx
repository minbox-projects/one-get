import { AppShell } from "./app/AppShell";
import { ErrorBoundary } from "./app/ErrorBoundary";

export default function App() {
  return (
    <ErrorBoundary>
      <AppShell />
    </ErrorBoundary>
  );
}
