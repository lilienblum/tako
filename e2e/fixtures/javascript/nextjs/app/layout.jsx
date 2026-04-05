export const metadata = {
  title: "Tako Next.js Fixture",
  description: "Next.js deploy fixture for Tako e2e coverage.",
};

export default function RootLayout({ children }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
