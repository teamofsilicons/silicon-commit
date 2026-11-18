"use client";
import { useQuery } from "@tanstack/react-query";
import { Input } from "@/components/silicon-ui/input/input";
import { Select } from "@/components/silicon-ui/select/select";
import { api } from "@/lib/client/api";
import { useResource } from "./common";
import s from "./product.module.css";
export type Board = {
  id: string;
  name: string;
  parent_id: string | null;
  owner_account: string | null;
  home_account: string | null;
  home_team: string | null;
  progress_mode: "count" | "time" | "hidden";
  revision: number;
  role: "admin" | "member";
  categories: { id: string; name: string }[];
  members: { id: string | null; subject: string; role: string }[];
};
export function BoardField({
  value,
  onChange,
}: {
  value: string;
  onChange: (id: string) => void;
}) {
  const data = useResource<{ items: Board[] }>("/v1/boards");
  return (
    <Select
      label="Board"
      value={value || "none"}
      onValueChange={(v) => onChange(v === "none" ? "" : v)}
      options={[
        { value: "none", label: "No board" },
        ...(data.data?.items ?? []).map((b) => ({
          value: b.id,
          label: b.name,
        })),
      ]}
    />
  );
}
export function CategoriesField({
  value,
  onChange,
  board,
}: {
  value: string;
  onChange: (v: string) => void;
  board: string;
}) {
  const info = useQuery({
    queryKey: ["/v1/boards/" + board],
    enabled: !!board,
    queryFn: ({ signal }) => api.get<Board>("/v1/boards/" + board, { signal }),
  });
  const selected = value
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean);
  return (
    <div className={s.field}>
      <Input
        label="Categories"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        description="Separate category names with commas."
      />
      {info.data?.categories.length ? (
        <div className={s.meta}>
          {info.data.categories.map((c) => (
            <label className={s.check} key={c.id}>
              <input
                type="checkbox"
                checked={selected.includes(c.name)}
                onChange={(e) =>
                  onChange(
                    (e.target.checked
                      ? [...selected, c.name]
                      : selected.filter((n) => n !== c.name)
                    ).join(", "),
                  )
                }
              />
              {c.name}
            </label>
          ))}
        </div>
      ) : null}
    </div>
  );
}
export function PlanFields({
  minutes,
  progress,
  onMinutes,
  onProgress,
}: {
  minutes: string;
  progress: string;
  onMinutes: (v: string) => void;
  onProgress: (v: string) => void;
}) {
  return (
    <div className={s.toolbar}>
      <Input
        label="Ideal time (minutes)"
        type="number"
        min={1}
        max={525600}
        value={minutes}
        onChange={(e) => onMinutes(e.target.value)}
      />
      <Input
        label="Progress (%)"
        type="number"
        min={0}
        max={100}
        value={progress}
        onChange={(e) => onProgress(e.target.value)}
      />
    </div>
  );
}
export function MultiSelect({
  label,
  options,
  values,
  onChange,
}: {
  label: string;
  options: { id: string; name: string }[];
  values: string[];
  onChange: (v: string[]) => void;
}) {
  return (
    <fieldset className={s.checks}>
      <legend>{label}</legend>
      {options.map((o) => (
        <label key={o.id}>
          <input
            type="checkbox"
            checked={values.includes(o.id)}
            onChange={(e) =>
              onChange(
                e.target.checked
                  ? [...values, o.id]
                  : values.filter((id) => id !== o.id),
              )
            }
          />
          {o.name}
        </label>
      ))}
      {!options.length ? <span className={s.muted}>None available</span> : null}
    </fieldset>
  );
}
