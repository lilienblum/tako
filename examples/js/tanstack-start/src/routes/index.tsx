import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/")({
  component: Home,
});

function Home() {
  return (
    <section>
      <h1>TanStack Start Basic</h1>
      <p>
        This example is based on TanStack Router&apos;s <code>start-basic</code> and writes Vite
        deploy metadata to <code>dist/.tako-vite.json</code> using <code>tako.sh/vite</code>.
      </p>
      <p>
        Deploy uses <code>dist/</code> as input, merges <code>assets</code> into
        <code> dist/public/</code>, and writes runtime metadata to archive <code>app.json</code>.
      </p>
    </section>
  );
}
