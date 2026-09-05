import { create } from "zustand";
import { persist } from "zustand/middleware";

interface UiPreferences {
  sidebarCollapsed: boolean;
  toggleSidebar: () => void;
}

export const useUiPreferences = create<UiPreferences>()(
  persist(
    (set) => ({
      sidebarCollapsed: false,
      toggleSidebar: () => set((state) => ({ sidebarCollapsed: !state.sidebarCollapsed }))
    }),
    { name: "aushadharth-ui-preferences" }
  )
);
