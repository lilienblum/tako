export const metadata = {
  title: "Tako Next.js Example",
  description: "Next.js example app deployed with Tako.",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
