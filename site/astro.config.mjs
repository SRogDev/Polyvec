import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

// https://astro.build/config
export default defineConfig({
  integrations: [
    starlight({
      title: "Polyvec",
      description:
        "A from-scratch Rust vector database for massive, cheap, high-recall reads of company knowledge by AI agents.",
      social: [{ icon: "github", label: "GitHub", href: "https://github.com/SRogDev/Polyvec" }],
      customCss: ["./src/styles/custom.css"],
      defaultLocale: "en",
      sidebar: [
        {
          label: "Start here",
          items: [
            { label: "Overview", slug: "index" },
            { label: "Installation", slug: "installation" },
            { label: "Quickstart", slug: "quickstart" },
            { label: "First collection, first query", slug: "first-query" },
          ],
        },
        {
          label: "Working with data",
          items: [
            { label: "API reference", slug: "api-reference" },
            { label: "Configuration & env vars", slug: "configuration" },
            { label: "Vector + lexical search", slug: "search/vector-lexical" },
            { label: "Hybrid retrieval + filtering", slug: "search/hybrid-filtering" },
            { label: "Embedding providers", slug: "data/embedding-providers" },
            { label: "Ingestion pipeline & formats", slug: "data/ingestion" },
            { label: "Canonical model, provenance & versioning", slug: "data/canonical-model" },
            { label: "Relationship-aware retrieval", slug: "data/relationships" },
          ],
        },
        {
          label: "Operations",
          items: [
            { label: "Persistence, backups & recovery", slug: "operations/persistence" },
            { label: "Performance & benchmarks", slug: "operations/performance" },
            { label: "Security & network exposure", slug: "operations/security" },
            { label: "Troubleshooting", slug: "operations/troubleshooting" },
          ],
        },
        {
          label: "Project",
          items: [
            { label: "Architecture & decisions", slug: "project/architecture" },
            { label: "AI integration prompt", slug: "project/ai-integration" },
            { label: "Limitations & roadmap", slug: "project/limitations" },
            { label: "Contributing & dev setup", slug: "project/contributing" },
          ],
        },
      ],
    }),
  ],
});
