import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Alert, Button, Modal, Space, Typography } from "antd";
import type { CommandResult } from "@/types/command";

interface MigrationStatus {
  available: boolean;
  eligible: boolean;
  message: string;
}

interface MigrationReport {
  message: string;
  credentials_migrated: number;
}

interface LegacyMigrationProps {
  onComplete: () => Promise<void>;
  onBusyChange: (busy: boolean) => void;
}

export function LegacyMigration({ onComplete, onBusyChange }: LegacyMigrationProps) {
  const [status, setStatus] = useState<MigrationStatus | null>(null);
  const [error, setError] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [completed, setCompleted] = useState<MigrationReport | null>(null);
  const [checking, setChecking] = useState(true);

  const inspect = async () => {
    setChecking(true);
    setError("");
    try {
      const result = await invoke<CommandResult<MigrationStatus>>("inspect_legacy_migration");
      if (!result.success || !result.data) throw new Error(result.error?.message ?? "旧数据检测失败");
      setStatus(result.data);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "旧数据检测失败");
    } finally {
      setChecking(false);
    }
  };

  useEffect(() => { void inspect(); }, []);

  const migrate = async () => {
    if (busy) return;
    setBusy(true);
    onBusyChange(true);
    setError("");
    let succeeded = false;
    try {
      const result = await invoke<CommandResult<MigrationReport>>("migrate_legacy_data", { confirmed: true });
      if (!result.success || !result.data) throw new Error(result.error?.message ?? "旧数据迁移失败");
      setCompleted(result.data);
      setConfirming(false);
      succeeded = true;
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "旧数据迁移失败，请检查后重试");
    } finally {
      setBusy(false);
      if (!succeeded) onBusyChange(false);
    }
  };

  if (completed) {
    return <Alert type="success" title="旧数据已复制到 OfferFlow"
      description={<Space orientation="vertical">
        <Typography.Text>{completed.message}</Typography.Text>
        <Typography.Text>已迁移模型凭据：{completed.credentials_migrated} 项。请重新登录招聘平台，并检查简历和图片文件路径。</Typography.Text>
        <Button type="primary" onClick={() => void onComplete()}>载入迁移后的配置</Button>
      </Space>} />;
  }

  return <>
    {checking && <Typography.Text type="secondary">正在检测旧版数据…</Typography.Text>}
    {!checking && error && !confirming && <Alert type="warning" title={error}
      action={<Button onClick={() => void inspect()}>重新检测</Button>} />}
    {status?.available && <Alert type={status.eligible ? "info" : "warning"}
      title="检测到旧版 FuckJob 数据"
      description={<Space orientation="vertical">
        <Typography.Text>{status.message}</Typography.Text>
        {status.eligible && <Button disabled={busy} onClick={() => setConfirming(true)}>迁移旧数据</Button>}
      </Space>} />}
    <Modal open={confirming} title="确认复制旧版数据"
      okText="确认迁移" cancelText="暂不迁移" confirmLoading={busy}
      closable={!busy} mask={{ closable: !busy }} keyboard={!busy}
      cancelButtonProps={{ disabled: busy }}
      onCancel={() => { if (!busy) setConfirming(false); }}
      onOk={() => void migrate()}>
      <Typography.Paragraph>
        请先关闭旧版应用。将复制旧配置、岗位、聊天、分析、简历文本及相关业务记录，
        并将旧模型凭据复制到独立凭据空间；不会删除旧数据或覆盖已有的新模型凭据。
      </Typography.Paragraph>
      <Typography.Paragraph>
        不复制浏览器登录态、模拟面试历史、PDF 和图片原文件。迁移后必须重新登录招聘平台，
        检查文件路径、求职方案和自动回复设置后，再手动启动任务。
      </Typography.Paragraph>
      {error && <Alert type="error" title={error} />}
    </Modal>
  </>;
}
