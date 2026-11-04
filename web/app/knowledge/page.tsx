"use client";
import { useCallback, useEffect, useState } from "react";
import { useFeedback } from "@/components/Feedback";
import { api, del, post } from "@/lib/api";
import { when } from "@/lib/fmt";
import { Card, Empty, MsgBox, Skeleton, useAction } from "@/lib/ui";

type Note = { id: number; title: string; content: string; source: string; enabled: boolean; updated: string; origin: string };

const ORIGIN: Record<string, string> = { seed: "预置", user: "我的", ai: "AI 观察" };

// Keep in step with the limit the server applies when it builds the model request.
const LIMIT = 24000;

function NoteCard({ note, busy, run, reload }: { note: Note; busy: string; run: ReturnType<typeof useAction>["run"]; reload: () => Promise<void> }) {
  const { confirm } = useFeedback();
  const [title, setTitle] = useState(note.title);
  const [content, setContent] = useState(note.content);
  const [source, setSource] = useState(note.source);
  const dirty = title !== note.title || content !== note.content || source !== note.source;

  async function save() {
    await run(`save-${note.id}`, async () => {
      await post(`/api/knowledge/${note.id}`, { title, content, source });
      await reload();
      return { kind: "good", text: `已保存「${title}」。` };
    });
  }

  async function toggle(enabled: boolean) {
    await run(`toggle-${note.id}`, async () => {
      await post(`/api/knowledge/${note.id}`, { enabled });
      await reload();
      return { kind: "good", text: enabled ? `已启用「${note.title}」，下一轮分析起参考。` : `已停用「${note.title}」。` };
    });
  }

  async function remove() {
    const ok = await confirm({ title: `删除「${note.title}」？`, body: "删除后 AI 不再参考这条笔记。", ok: "删除", danger: true });
    if (!ok) return;
    await run(`del-${note.id}`, async () => {
      await del(`/api/knowledge/${note.id}`);
      await reload();
      return { kind: "good", text: "已删除。" };
    });
  }

  return (
    <div className="kb-note">
      <div className="kb-head">
        <label className="toggle">
          <input type="checkbox" checked={note.enabled} disabled={busy === `toggle-${note.id}`} onChange={(e) => toggle(e.target.checked)} />
          <span>{note.enabled ? "AI 会参考" : "已停用"}</span>
        </label>
        <span className="muted">
          {ORIGIN[note.origin] || note.origin} · 更新于 {when(note.updated)}
        </span>
      </div>
      <input value={title} maxLength={80} aria-label="标题" onChange={(e) => setTitle(e.target.value)} />
      <textarea value={content} maxLength={4000} rows={Math.min(14, Math.max(5, content.split("\n").length + 1))} aria-label="内容" onChange={(e) => setContent(e.target.value)} />
      <input value={source} maxLength={600} placeholder="来源（可留空）" aria-label="来源" onChange={(e) => setSource(e.target.value)} />
      <div className="row">
        <button onClick={save} disabled={!dirty || !title.trim() || !content.trim() || busy === `save-${note.id}`}>
          {busy === `save-${note.id}` ? "保存中…" : "保存修改"}
        </button>
        <button className="ghost" onClick={remove} disabled={busy === `del-${note.id}`}>
          删除
        </button>
      </div>
    </div>
  );
}

export default function KnowledgePage() {
  const { busy, msg, run } = useAction();
  const [notes, setNotes] = useState<Note[] | null>(null);
  const [err, setErr] = useState("");
  const [title, setTitle] = useState("");
  const [content, setContent] = useState("");
  const [source, setSource] = useState("");

  const load = useCallback(async () => {
    try {
      setNotes(await api<Note[]>("/api/knowledge"));
      setErr("");
    } catch (e: any) {
      setErr(e?.message || "加载失败");
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function add() {
    await run("add", async () => {
      await post("/api/knowledge", { title, content, source });
      setTitle("");
      setContent("");
      setSource("");
      await load();
      return { kind: "good", text: "已添加，下一轮分析起 AI 会参考它。" };
    });
  }

  const used = (notes || []).filter((n) => n.enabled).reduce((s, n) => s + [...n.title].length + [...n.content].length, 0);

  return (
    <>
      <Card title="知识库" note={notes ? `${notes.filter((n) => n.enabled).length} / ${notes.length} 条启用 · 约 ${used.toLocaleString()} / ${LIMIT.toLocaleString()} 字` : ""}>
        <p className="hint">
          这里的笔记会随每一轮分析发给 AI，当作策略参考。标为“AI 观察”的是 AI 复盘自己的交易后写的，只有它能改这些（你也可以编辑、停用或删除）。它们只是启发，不是规则：不会覆盖风控限制，AI 会在分析里说明参考或否决了哪几条。预置的几条来自公开资料，多为博客和教程，证据强弱不一，各条里已注明；你可以改、停用或删除，也可以加自己的经验。启用的内容总量超过上限时，靠后的笔记不会被发送。
        </p>
        <MsgBox m={msg} />
        {err ? <div className="msg err">{err}</div> : null}
      </Card>

      <Card title="新增笔记" className="kb">
        <input value={title} maxLength={80} placeholder="标题，例如：突破后回踩再进" aria-label="新笔记标题" onChange={(e) => setTitle(e.target.value)} />
        <textarea value={content} maxLength={4000} rows={5} placeholder="内容：条件、做法、注意事项、证据强弱……" aria-label="新笔记内容" onChange={(e) => setContent(e.target.value)} />
        <input value={source} maxLength={600} placeholder="来源（可留空）" aria-label="新笔记来源" onChange={(e) => setSource(e.target.value)} />
        <div className="row">
          <button onClick={add} disabled={!title.trim() || !content.trim() || busy === "add"}>
            {busy === "add" ? "添加中…" : "添加"}
          </button>
        </div>
      </Card>

      <Card title="全部笔记" className="kb">
        {!notes ? (
          <Skeleton lines={6} />
        ) : notes.length ? (
          notes.map((n) => <NoteCard key={`${n.id}-${n.updated}`} note={n} busy={busy} run={run} reload={load} />)
        ) : (
          <Empty>还没有笔记。</Empty>
        )}
      </Card>
    </>
  );
}
