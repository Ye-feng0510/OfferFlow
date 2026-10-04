import { StrictMode, type ReactNode } from "react";
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AutoUpdater, AutoUpdaterModal, UpdaterProvider, useUpdater } from "./updater";

const mocks = vi.hoisted(() => ({
  info: vi.fn(),
  check: vi.fn(),
  relaunch: vi.fn(),
  invoke: vi.fn(),
  isTauri: vi.fn(),
}));

vi.mock("antd", () => ({ message: { info: mocks.info } }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: mocks.relaunch }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
  isTauri: mocks.isTauri,
}));

function Wrapper({ children }: { children: ReactNode }) {
  return (
    <StrictMode>
      <UpdaterProvider>
        <AutoUpdater />
        <AutoUpdaterModal />
        {children}
      </UpdaterProvider>
    </StrictMode>
  );
}

describe.each([true, false])("disabled updater (desktop=%s)", (desktop) => {
  const fetchMock = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    mocks.isTauri.mockReturnValue(desktop);
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    expect(mocks.check).not.toHaveBeenCalled();
    expect(mocks.invoke).not.toHaveBeenCalled();
    expect(mocks.relaunch).not.toHaveBeenCalled();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("does not check on mount or during a silent check", async () => {
    const { result } = renderHook(() => useUpdater(), { wrapper: Wrapper });
    expect(result.current).toMatchObject({
      checking: false,
      update: null,
      downloading: false,
      progress: { downloaded: 0 },
      error: "",
    });

    await act(async () => {
      expect(await result.current.checkForUpdates()).toBeNull();
      expect(await result.current.checkForUpdates(false)).toBeNull();
    });
    expect(mocks.info).not.toHaveBeenCalled();
  });

  it("honestly reports that automatic updates are disabled on manual check", async () => {
    const { result } = renderHook(() => useUpdater(), { wrapper: Wrapper });
    await act(async () => {
      expect(await result.current.checkForUpdates(true)).toBeNull();
    });
    expect(mocks.info).toHaveBeenCalledExactlyOnceWith("独立发行版本暂未启用自动更新");
    expect(result.current.checking).toBe(false);
  });

  it("keeps install and dismiss inert without changing state", async () => {
    const { result } = renderHook(() => useUpdater(), { wrapper: Wrapper });
    const original = result.current;
    await act(async () => {
      await result.current.install();
      result.current.dismissUpdate();
    });
    expect(result.current).toBe(original);
    expect(result.current.update).toBeNull();
    expect(result.current.downloading).toBe(false);
    expect(mocks.info).not.toHaveBeenCalled();
  });
});
