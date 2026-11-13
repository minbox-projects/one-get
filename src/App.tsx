import { AppShell } from "./app/AppShell";
import { ErrorBoundary } from "./app/ErrorBoundary";
import { I18nProvider } from "./i18n";

export default function App() {
  return (
    <ErrorBoundary>
      <I18nProvider language="system">
        <AppShell />
      </I18nProvider>
    </ErrorBoundary>
  );
}
