// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import starlightThemeNova from "starlight-theme-nova";
// import starlightVersions from "starlight-versions";

import icon from "astro-icon";

// https://astro.build/config
export default defineConfig({
  site: "https://pwnwrkz.github.io",
  base: "/tungsten",
  integrations: [
    starlight({
      plugins: [
        starlightThemeNova()
      ],
      /*components: {
        Search: "starlight-versions/components/VersionSearch.astro",
        ThemeSelect: "./src/components/VersionThemeSelect.astro",
        },*/
      title: "Tungsten",
      favicon: "/favicon.svg",
      customCss: ["./src/styles/custom.css"],
      logo: {
        light: "./src/assets/TungstenBannerDark.svg",
        dark: "./src/assets/TungstenBannerLight.svg",
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
          label: "Guides",
          items: [
            { label: "Introduction", slug: "guides/introduction" },
            { label: "Installation", slug: "guides/installation" },
            { label: "Initializing Your Project", slug: "guides/init" },
            { label: "Actually Syncing", slug: "guides/sync" },
            { label: "Packing Spritesheets", slug: "guides/pack" },
            { label: "Stuff about images and audio", slug: "guides/extra" },
            { label: "Metadata Files", slug: "guides/meta" },
          ],
        },
        {
          label: "Reference",
          items: [
            { label: "CLI", slug: "reference/cli" },
            { label: "Configuration", slug: "reference/configuration" },
            { label: "Meta Files", slug: "reference/meta-files" },
          ],
        },
      ],
    }),
    icon(),
  ],
});
