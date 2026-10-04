import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { LegacyMigration } from "./LegacyMigration";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const available = { success: true, data: { available: true, eligible: true, message: "可复制旧数据" }, error: null };

describe("LegacyMigration", () => {
  beforeEach(() => vi.mocked(invoke).mockReset());
  afterEach(cleanup);

  it("only inspects until the user explicitly confirms migration", async () => {
    vi.mocked(invoke).mockResolvedValue(available);
    render(<LegacyMigration onComplete={vi.fn()} onBusyChange={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "迁移旧数据" }));
    expect(screen.getByText(/不复制浏览器登录态/)).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("inspect_legacy_migration");
    fireEvent.click(screen.getByRole("button", { name: "暂不迁移" }));
    expect(invoke).not.toHaveBeenCalledWith("migrate_legacy_data", { confirmed: true });
  });

  it("keeps onboarding locked until the imported configuration is loaded", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(available).mockResolvedValueOnce({
      success: true, data: { message: "源数据已保留", credentials_migrated: 2 }, error: null,
    });
    const onComplete = vi.fn().mockResolvedValue(undefined);
    const onBusyChange = vi.fn();
    render(<LegacyMigration onComplete={onComplete} onBusyChange={onBusyChange} />);
    fireEvent.click(await screen.findByRole("button", { name: "迁移旧数据" }));
    fireEvent.click(screen.getByRole("button", { name: "确认迁移" }));
    expect(await screen.findByText("旧数据已复制到 OfferFlow")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("migrate_legacy_data", { confirmed: true });
    expect(onBusyChange).toHaveBeenLastCalledWith(true);
    expect(onComplete).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "载入迁移后的配置" }));
    await waitFor(() => expect(onComplete).toHaveBeenCalledOnce());
  });

  it("shows migration failures without reloading and permits a retry", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(available).mockResolvedValue({
      success: false, data: null, error: { message: "目标目录已有数据" },
    });
    const onComplete = vi.fn();
    const onBusyChange = vi.fn();
    render(<LegacyMigration onComplete={onComplete} onBusyChange={onBusyChange} />);
    fireEvent.click(await screen.findByRole("button", { name: "迁移旧数据" }));
    fireEvent.click(screen.getByRole("button", { name: "确认迁移" }));
    expect(await screen.findByText("目标目录已有数据")).toBeInTheDocument();
    expect(onBusyChange).toHaveBeenLastCalledWith(false);
    expect(onComplete).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.getByRole("button", { name: /确认迁移/ })).not.toHaveClass("ant-btn-loading"));
    fireEvent.click(screen.getByRole("button", { name: /确认迁移/ }));
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(3));
  });

  it("does not offer import when destination data already exists", async () => {
    vi.mocked(invoke).mockResolvedValue({
      success: true, data: { available: true, eligible: false, message: "已使用新版本，请勿覆盖" }, error: null,
    });
    render(<LegacyMigration onComplete={vi.fn()} onBusyChange={vi.fn()} />);
    expect(await screen.findByText("已使用新版本，请勿覆盖")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "迁移旧数据" })).not.toBeInTheDocument();
  });

  it("allows retrying a failed inspection without starting import", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("无法读取旧目录")).mockResolvedValueOnce(available);
    render(<LegacyMigration onComplete={vi.fn()} onBusyChange={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "重新检测" }));
    expect(await screen.findByRole("button", { name: "迁移旧数据" })).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith("migrate_legacy_data", { confirmed: true });
  });
});
