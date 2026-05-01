// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import starlightThemeNova from "starlight-theme-nova";

// https://astro.build/config
export default defineConfig({
  site: "https://pwnwrkz.github.io",
  base: "/tungsten",
  integrations: [
    starlight({
      plugins: [starlightThemeNova()],
      title: "Tungsten",
      favicon: "/favicon.svg",
      customCss: ["./src/styles/custom.css"],
      logo: {
        light: "./src/assets/TungstenBannerDark.png",
        dark: "./src/assets/TungstenBannerLight.png",
        replacesTitle: true,
      },
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/pwnwrkz/tungsten",
        },
      ],
      sidebar: [
        {
          label: "Getting Started",
          items: [
            { label: "Introduction", slug: "getting-started/introduction" },
            { label: "Installation", slug: "getting-started/installation" },
          ],
        },
        {
          label: "Tutorials",
          items: [
            { label: "Your First Sync", slug: "guides/first-sync" },
            { label: "Mastering Asset Packing", slug: "guides/packing" },
            { label: "Advanced Workflows", slug: "guides/advanced" },
          ],
        },
        {
          label: "Reference",
          autogenerate: { directory: "reference" },
        },
      ],
    }),
  ],
});
