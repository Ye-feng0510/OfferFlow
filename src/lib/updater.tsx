import { createContext, useContext, type ReactNode } from "react";
import type { Update } from "@tauri-apps/plugin-updater";
import { message } from "antd";

type DownloadState = {
  downloaded: number;
  total?: number;
};

interface UpdaterContextType {
  checking: boolean;
  update: Update | null;
  downloading: boolean;
  progress: DownloadState;
  error: string;
  checkForUpdates: (manual?: boolean) => Promise<Update | null>;
  install: () => Promise<void>;
  dismissUpdate: () => void;
}

const UpdaterContext = createContext<UpdaterContextType | null>(null);

// Keep the consumer API, but do not load an updater client or contact any release
// endpoint until this independent distribution has its own signing infrastructure.
const disabledUpdater: UpdaterContextType = {
  checking: false,
  update: null,
  downloading: false,
  progress: { downloaded: 0 },
  error: "",
  checkForUpdates: async (manual = false) => {
    if (manual) {
      void message.info("独立发行版本暂未启用自动更新");
    }
    return null;
  },
  install: async () => {},
  dismissUpdate: () => {},
};

export function UpdaterProvider({ children }: { children: ReactNode }) {
  return <UpdaterContext.Provider value={disabledUpdater}>{children}</UpdaterContext.Provider>;
}

export function useUpdater() {
  const context = useContext(UpdaterContext);
  if (!context) {
    throw new Error("useUpdater 必须在 UpdaterProvider 内部使用");
  }
  return context;
}

export function AutoUpdaterModal() {
  return null;
}

export function AutoUpdater() {
  return null;
}
