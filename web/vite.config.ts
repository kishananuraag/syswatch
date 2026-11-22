import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Relative base so the built files work when served from any folder.
export default defineConfig({
  base: "./",
  plugins: [react()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
