import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/")({
  component: Home,
});

function Home() {
  return (
    <section>
      <h1>TanStack Start Basic</h1>
      <p>
        This example is based on TanStack Router&apos;s <code>start-basic</code> and stages deploy
        output into <code>.tako/artifacts/app</code> using <code>tako.sh/vite</code>.
      </p>
      <p>
        Build output from <code>.output/public</code> is merged with <code>public/</code> into
        <code> static/</code>, and <code>.output/server</code> is copied into
        <code> server/</code>.
      </p>
    </section>
  );
}
