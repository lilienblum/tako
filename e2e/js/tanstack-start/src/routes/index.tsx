import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/")({
  component: Home,
});

function Home() {
  return (
    <section>
      <h1>TanStack Start E2E Fixture</h1>
      <p>This fixture is used by deploy e2e Docker tests.</p>
    </section>
  );
}
