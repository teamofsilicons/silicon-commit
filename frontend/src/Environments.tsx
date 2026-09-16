import { TestingLogin } from "./Testing";

export function Environments() {
  return (
    <>
      <header class="page-header">
        <div><h1>Testing environments</h1>
          <p class="muted">Create and manage shared environments in Honeycomb. Select one here using Commit’s application secret.</p>
        </div>
        <a class="button" href="https://honeycomb.teamofsilicons.com" target="_blank" rel="noopener noreferrer">Open Honeycomb ↗</a>
      </header>
      <TestingLogin />
      <section class="panel">
        <h3>Environment lifecycle</h3>
        <p>Honeycomb coordinates cleaning, disabling, restoring, and permanently removing environments across applications. Cleaning removes Commit’s sandbox content; restoring does not bring cleaned data back.</p>
        <p>Sign in with a test SLT or an existing sandbox identity after selecting the application secret. Your actions use that identity’s permissions.</p>
      </section>
    </>
  );
}
