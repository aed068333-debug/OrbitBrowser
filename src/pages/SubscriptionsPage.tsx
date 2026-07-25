import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  CheckSquare,
  Clock3,
  Gauge,
  Link2,
  Plus,
  RefreshCw,
  Search,
  Square,
  SquareStack,
  Trash2,
  UploadCloud,
  Zap,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";

import { Button } from "@/components/Button";
import { EmptyState } from "@/components/EmptyState";
import { SelectField, TextField, TextareaField } from "@/components/FormField";
import { Modal } from "@/components/Modal";
import { useI18n } from "@/i18n";
import { errorMessage, formatDateTime } from "@/lib/format";
import { toastError, toastSuccess } from "@/stores/toastStore";
import { browserApi } from "@/lib/tauri";
import { useUiStore } from "@/stores/uiStore";
import type {
  Environment,
  ProxySubscription,
  ProxySubscriptionNode,
  SaveProxySubscriptionInput,
} from "@/types/domain";

function defaultDraft(): SaveProxySubscriptionInput {
  return {
    name: "",
    url: "",
    enabled: true,
    auto_sync_interval_minutes: 0,
  };
}

function latencyTone(latency?: number | null) {
  if (latency == null) {
    return "border-line bg-ink-50 text-ink-500";
  }
  if (latency < 120) {
    return "border-ok/20 bg-green-50 text-ok";
  }
  if (latency < 260) {
    return "border-brand-500/20 bg-blue-50 text-brand-600";
  }
  if (latency < 450) {
    return "border-warn/20 bg-amber-50 text-warn";
  }
  return "border-danger/20 bg-red-50 text-danger";
}

function protocolTone(protocol: string) {
  switch (protocol) {
    case "vless":
      return "border-brand-500/20 bg-blue-50 text-brand-600";
    case "vmess":
      return "border-brand-500/20 bg-brand-50 text-brand-700";
    case "trojan":
      return "border-warn/20 bg-amber-50 text-warn";
    case "shadowsocks":
      return "border-ok/20 bg-green-50 text-ok";
    case "hysteria2":
      return "border-danger/20 bg-red-50 text-danger";
    default:
      return "border-line bg-ink-50 text-ink-600";
  }
}

export function SubscriptionsPage() {
  const queryClient = useQueryClient();
  const { copy, format, language } = useI18n();
  const text = copy.subscriptions;
  const setHeaderActions = useUiStore((state) => state.setHeaderActions);
  const [draft, setDraft] = useState<SaveProxySubscriptionInput | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [applyNodeId, setApplyNodeId] = useState<string | null>(null);
  const [selectedEnvironmentId, setSelectedEnvironmentId] = useState("");
  const [selectedNodeIds, setSelectedNodeIds] = useState<string[]>([]);
  const [nodeSearch, setNodeSearch] = useState("");

  const subscriptionsQuery = useQuery({
    queryKey: ["proxy-subscriptions"],
    queryFn: browserApi.listProxySubscriptions,
  });
  const nodesQuery = useQuery({
    queryKey: ["proxy-subscription-nodes", selectedId],
    queryFn: () =>
      browserApi.listProxySubscriptionNodes(selectedId ?? undefined),
    enabled: Boolean(selectedId),
  });
  const environmentsQuery = useQuery({
    queryKey: ["environments"],
    queryFn: browserApi.listEnvironments,
  });
  const subscriptions = subscriptionsQuery.data ?? [];
  const selected =
    subscriptions.find((item) => item.id === selectedId) ??
    subscriptions[0] ??
    null;
  const activeSubscriptionId = selected?.id ?? null;

  useEffect(() => {
    if (!selectedId && selected) {
      setSelectedId(selected.id);
    }
  }, [selected, selectedId]);

  useEffect(() => {
    setSelectedNodeIds([]);
    setNodeSearch("");
  }, [activeSubscriptionId]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      void browserApi
        .syncDueProxySubscriptions()
        .then(async (results) => {
          if (results.length > 0) {
            await queryClient.invalidateQueries({
              queryKey: ["proxy-subscriptions"],
            });
            await queryClient.invalidateQueries({
              queryKey: ["proxy-subscription-nodes"],
            });
          }
        })
        .catch(() => undefined);
    }, 60_000);
    void browserApi.syncDueProxySubscriptions().catch(() => undefined);
    return () => window.clearInterval(timer);
  }, [queryClient]);

  const nodes = useMemo(() => {
    const list = nodesQuery.data ?? [];
    if (!activeSubscriptionId) {
      return list;
    }
    return list.filter((node) => node.subscription_id === activeSubscriptionId);
  }, [activeSubscriptionId, nodesQuery.data]);

  const usableNodes = useMemo(() => {
    const keyword = nodeSearch.trim().toLowerCase();
    return nodes.filter((node) => {
      if (node.is_meta) {
        return false;
      }
      if (!keyword) {
        return true;
      }
      return [
        node.name,
        node.protocol,
        node.server,
        String(node.port),
        ...(node.tags ?? []),
      ]
        .join(" ")
        .toLowerCase()
        .includes(keyword);
    });
  }, [nodeSearch, nodes]);

  const metaNodes = nodes.filter((node) => node.is_meta);
  const environments = environmentsQuery.data ?? [];
  const measuredCount = usableNodes.filter(
    (node) => node.latency_ms != null,
  ).length;
  const autoSyncCount = subscriptions.filter(
    (item) => item.auto_sync_interval_minutes > 0,
  ).length;
  const selectedCount = selectedNodeIds.length;
  const allFilteredSelected =
    usableNodes.length > 0 &&
    usableNodes.every((node) => selectedNodeIds.includes(node.id));
  const selectedUsableIds =
    selectedNodeIds.length > 0
      ? selectedNodeIds
      : usableNodes.map((node) => node.id);

  const invalidate = async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ["proxy-subscriptions"] }),
      queryClient.invalidateQueries({ queryKey: ["proxy-subscription-nodes"] }),
      queryClient.invalidateQueries({ queryKey: ["environments"] }),
    ]);
  };

  const saveMutation = useMutation({
    mutationFn: browserApi.saveProxySubscription,
    onSuccess: async (subscription) => {
      setDraft(null);
      setSelectedId(subscription.id);
      toastSuccess(copy.common.save);
      await invalidate();
    },
    onError: (error) => toastError(errorMessage(error)),
  });

  const deleteMutation = useMutation({
    mutationFn: browserApi.deleteProxySubscription,
    onSuccess: async () => {
      setSelectedId(null);
      toastSuccess(copy.common.delete);
      await invalidate();
    },
    onError: (error) => toastError(errorMessage(error)),
  });

  const syncMutation = useMutation({
    mutationFn: browserApi.syncProxySubscription,
    onSuccess: async (result) => {
      setSelectedId(result.subscription.id);
      toastSuccess(
        format(text.syncResult, {
          imported: result.imported,
          skipped: result.skipped,
        }),
      );
      await invalidate();
    },
    onError: (error) => toastError(errorMessage(error)),
  });

  const applyMutation = useMutation({
    mutationFn: browserApi.applySubscriptionNodeToEnvironment,
    onSuccess: async () => {
      setApplyNodeId(null);
      setSelectedEnvironmentId("");
      toastSuccess(text.applySuccess);
      await invalidate();
    },
    onError: (error) => toastError(errorMessage(error)),
  });

  const createMutation = useMutation({
    mutationFn: browserApi.createEnvironmentsFromSubscriptionNodes,
    onSuccess: async (result) => {
      toastSuccess(
        format(text.createEnvironmentsDone, {
          created: result.created.length,
          skipped: result.skipped,
        }),
      );
      await invalidate();
    },
    onError: (error) => toastError(errorMessage(error)),
  });

  const probeMutation = useMutation({
    mutationFn: browserApi.probeSubscriptionNodes,
    onSuccess: async (result) => {
      const ok = result.results.filter((item) => item.ok).length;
      const fail = result.results.length - ok;
      toastSuccess(
        format(text.probeDone, {
          ok,
          fail,
        }),
      );
      await queryClient.invalidateQueries({
        queryKey: ["proxy-subscription-nodes"],
      });
    },
    onError: (error) => toastError(errorMessage(error)),
  });

  const busy =
    syncMutation.isPending ||
    probeMutation.isPending ||
    createMutation.isPending ||
    applyMutation.isPending ||
    saveMutation.isPending ||
    deleteMutation.isPending;

  useEffect(() => {
    setHeaderActions(
      <>
        <div className="hidden items-center divide-x divide-line rounded-md border border-line bg-ink-50 px-1 text-xs text-ink-500 xl:flex">
          <span className="px-2.5 py-1.5">
            {text.metrics.subscriptions}: {subscriptions.length}
          </span>
          <span className="px-2.5 py-1.5">
            {text.metrics.nodes}: {selected?.node_count ?? 0}
          </span>
          <span className="px-2.5 py-1.5">
            {text.metrics.autoSync}: {autoSyncCount}
          </span>
        </div>
        <Button
          icon={<RefreshCw className="h-4 w-4" />}
          onClick={() => {
            void subscriptionsQuery.refetch();
            void nodesQuery.refetch();
          }}
        >
          {copy.common.refresh}
        </Button>
        <Button
          icon={<Plus className="h-4 w-4" />}
          onClick={() => setDraft(defaultDraft())}
          variant="primary"
        >
          {text.addSubscription}
        </Button>
      </>,
    );
    return () => setHeaderActions(undefined);
  }, [
    autoSyncCount,
    copy.common.refresh,
    nodesQuery,
    selected?.node_count,
    setHeaderActions,
    subscriptions.length,
    subscriptionsQuery,
    text.addSubscription,
    text.metrics.autoSync,
    text.metrics.nodes,
    text.metrics.subscriptions,
  ]);

  const openEdit = (subscription: ProxySubscription) =>
    setDraft({
      id: subscription.id,
      name: subscription.name,
      url: subscription.url,
      enabled: subscription.enabled,
      auto_sync_interval_minutes: subscription.auto_sync_interval_minutes ?? 0,
    });

  const toggleNode = (nodeId: string) => {
    setSelectedNodeIds((current) =>
      current.includes(nodeId)
        ? current.filter((item) => item !== nodeId)
        : [...current, nodeId],
    );
  };

  return (
    <div className="viewport-page grid-rows-[auto_minmax(0,1fr)]">
      <section className="panel shrink-0 overflow-hidden p-3">
        <div className="flex flex-wrap items-center gap-2.5">
          <div className="relative min-w-[240px] flex-1 md:max-w-xl">
            <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-ink-500" />
            <input
              className="control-focus h-9 w-full rounded-md border border-line bg-white pl-9 pr-3 text-sm"
              onChange={(event) => setNodeSearch(event.target.value)}
              placeholder={text.searchNodesPlaceholder}
              value={nodeSearch}
            />
          </div>
          <div className="flex flex-wrap items-center gap-2 text-xs text-ink-500">
            <span className="inline-flex items-center gap-1.5 rounded-md border border-line bg-ink-50 px-2.5 py-1.5">
              <Link2 className="h-3.5 w-3.5" />
              {selected?.name ?? text.emptyTitle}
            </span>
            <span className="inline-flex items-center gap-1.5 rounded-md border border-line bg-ink-50 px-2.5 py-1.5">
              <Gauge className="h-3.5 w-3.5" />
              {measuredCount}/{usableNodes.length}
            </span>
          </div>
        </div>
      </section>

      <div className="grid min-h-0 gap-4 lg:grid-cols-[minmax(0,300px)_minmax(0,1fr)]">
        <section className="panel flex min-h-0 min-w-0 flex-col overflow-hidden">
          <div className="border-b border-line px-4 py-3">
            <div className="text-sm font-semibold text-ink-900">
              {text.listTitle}
            </div>
            <div className="mt-1 text-xs leading-5 text-ink-500">
              {text.listHint}
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-auto p-3">
            {subscriptionsQuery.isError ? (
              <div className="rounded-lg border border-danger/20 bg-red-50 px-3 py-2 text-sm text-danger">
                {errorMessage(subscriptionsQuery.error)}
              </div>
            ) : null}

            {subscriptions.length === 0 ? (
              <EmptyState
                action={
                  <Button
                    icon={<Plus className="h-4 w-4" />}
                    onClick={() => setDraft(defaultDraft())}
                    variant="primary"
                  >
                    {text.addSubscription}
                  </Button>
                }
                description={text.emptyDescription}
                icon={<Link2 className="h-4 w-4" />}
                title={text.emptyTitle}
              />
            ) : (
              <div className="grid min-w-0 gap-2">
                {subscriptions.map((subscription) => {
                  const active = subscription.id === activeSubscriptionId;
                  return (
                    <button
                      className={`group w-full max-w-full overflow-hidden rounded-xl border px-3.5 py-3 text-left transition ${
                        active
                          ? "border-brand-500/35 bg-brand-50 shadow-[inset_0_0_0_1px_rgb(var(--color-brand-500)/0.08)]"
                          : "border-line bg-white hover:border-ink-300 hover:bg-ink-50"
                      }`}
                      key={subscription.id}
                      onClick={() => setSelectedId(subscription.id)}
                      type="button"
                    >
                      <div className="flex min-w-0 items-start justify-between gap-3">
                        <div className="min-w-0 flex-1 overflow-hidden">
                          <div className="truncate text-sm font-semibold text-ink-900">
                            {subscription.name}
                          </div>
                          <div className="mt-1 break-all text-xs leading-5 text-ink-500 line-clamp-2">
                            {subscription.url}
                          </div>
                        </div>
                        <span className="inline-flex h-6 shrink-0 items-center rounded-md border border-line bg-white px-2 text-xs font-medium text-ink-600">
                          {subscription.node_count}
                        </span>
                      </div>
                      <div className="mt-3 flex flex-wrap gap-1.5">
                        <span className="inline-flex items-center gap-1 rounded-md border border-line bg-ink-50 px-2 py-1 text-[11px] text-ink-500">
                          <Clock3 className="h-3 w-3" />
                          {subscription.last_synced_at
                            ? formatDateTime(
                                subscription.last_synced_at,
                                language,
                              )
                            : text.neverSynced}
                        </span>
                        <span className="inline-flex items-center gap-1 rounded-md border border-line bg-ink-50 px-2 py-1 text-[11px] text-ink-500">
                          <RefreshCw className="h-3 w-3" />
                          {subscription.auto_sync_interval_minutes > 0
                            ? format(text.autoSyncMinutes, {
                                minutes:
                                  subscription.auto_sync_interval_minutes,
                              })
                            : text.autoSyncOff}
                        </span>
                        {subscription.last_error ? (
                          <span className="inline-flex items-center rounded-md border border-danger/20 bg-red-50 px-2 py-1 text-[11px] text-danger">
                            {text.syncFailed}
                          </span>
                        ) : null}
                      </div>
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        </section>

        <section className="panel flex min-h-0 min-w-0 flex-col overflow-hidden">
          {selected ? (
            <>
              <div className="border-b border-line px-4 py-3">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <h3 className="text-[15px] font-semibold text-ink-900">
                        {selected.name}
                      </h3>
                      <span
                        className={`inline-flex h-6 items-center rounded-md border px-2 text-xs font-medium ${
                          selected.enabled
                            ? "border-ok/20 bg-green-50 text-ok"
                            : "border-line bg-ink-50 text-ink-500"
                        }`}
                      >
                        {selected.enabled
                          ? text.statusEnabled
                          : text.statusDisabled}
                      </span>
                    </div>
                    <div className="mt-1 break-all text-xs leading-5 text-ink-500">
                      {selected.url}
                    </div>
                    <div className="mt-2 flex flex-wrap gap-2 text-xs text-ink-500">
                      <span>
                        {text.nodeCount}: {selected.node_count}
                      </span>
                      <span>
                        {text.lastSynced}:{" "}
                        {selected.last_synced_at
                          ? formatDateTime(selected.last_synced_at, language)
                          : text.neverSynced}
                      </span>
                      <span>
                        {text.autoSync}:{" "}
                        {selected.auto_sync_interval_minutes > 0
                          ? format(text.autoSyncMinutes, {
                              minutes: selected.auto_sync_interval_minutes,
                            })
                          : text.autoSyncOff}
                      </span>
                    </div>
                    {selected.last_error ? (
                      <div className="mt-2 text-xs text-danger">
                        {selected.last_error}
                      </div>
                    ) : null}
                  </div>

                  <div className="flex flex-wrap items-center gap-2">
                    <Button onClick={() => openEdit(selected)} size="sm">
                      {copy.common.edit}
                    </Button>
                    <Button
                      disabled={busy}
                      icon={<RefreshCw className="h-4 w-4" />}
                      onClick={() => syncMutation.mutate(selected.id)}
                      size="sm"
                      variant="primary"
                    >
                      {text.sync}
                    </Button>
                    <Button
                      disabled={busy || usableNodes.length === 0}
                      icon={<Gauge className="h-4 w-4" />}
                      onClick={() =>
                        probeMutation.mutate({
                          subscription_id: selected.id,
                          node_ids:
                            selectedNodeIds.length > 0
                              ? selectedNodeIds
                              : undefined,
                          only_usable: true,
                        })
                      }
                      size="sm"
                    >
                      {probeMutation.isPending
                        ? text.probingLatency
                        : text.probeLatency}
                    </Button>
                    <Button
                      disabled={busy || selectedUsableIds.length === 0}
                      icon={<SquareStack className="h-4 w-4" />}
                      onClick={() => {
                        if (!window.confirm(text.createEnvironmentsConfirm)) {
                          return;
                        }
                        createMutation.mutate({
                          subscription_id: selected.id,
                          node_ids: selectedUsableIds,
                          only_usable: true,
                          group_id: selected.name,
                          name_prefix: selected.name,
                        });
                      }}
                      size="sm"
                    >
                      {text.createEnvironments}
                    </Button>
                    <Button
                      disabled={busy}
                      icon={<Trash2 className="h-4 w-4" />}
                      onClick={() => {
                        if (window.confirm(text.deleteConfirm)) {
                          deleteMutation.mutate(selected.id);
                        }
                      }}
                      size="sm"
                      variant="danger"
                    >
                      {copy.common.delete}
                    </Button>
                  </div>
                </div>
              </div>

              <div className="flex min-h-12 flex-wrap items-center gap-2 border-b border-line bg-ink-50 px-4 py-2.5">
                <Button
                  icon={
                    allFilteredSelected ? (
                      <CheckSquare className="h-4 w-4" />
                    ) : (
                      <Square className="h-4 w-4" />
                    )
                  }
                  onClick={() =>
                    setSelectedNodeIds((current) =>
                      allFilteredSelected
                        ? current.filter(
                            (id) => !usableNodes.some((node) => node.id === id),
                          )
                        : Array.from(
                            new Set([
                              ...current,
                              ...usableNodes.map((node) => node.id),
                            ]),
                          ),
                    )
                  }
                  size="sm"
                >
                  {allFilteredSelected
                    ? text.clearSelection
                    : text.selectAllUsable}
                </Button>
                <span className="mr-auto text-xs font-medium text-ink-500">
                  {format(text.selectedCount, { count: selectedCount })}
                </span>
                {selectedCount > 0 ? (
                  <Button
                    onClick={() => setSelectedNodeIds([])}
                    size="sm"
                    variant="ghost"
                  >
                    {text.clearSelection}
                  </Button>
                ) : null}
              </div>

              <div className="min-h-0 flex-1 overflow-auto">
                {metaNodes.length > 0 ? (
                  <div className="flex flex-wrap gap-2 border-b border-line px-4 py-3">
                    {metaNodes.map((node) => (
                      <div
                        className="inline-flex max-w-full items-center gap-1.5 rounded-full border border-line bg-ink-50 px-2.5 py-1 text-xs text-ink-600"
                        key={node.id}
                        title={node.name}
                      >
                        <Zap className="h-3 w-3 shrink-0 text-ink-400" />
                        <span className="max-w-[16rem] truncate">{node.name}</span>
                      </div>
                    ))}
                  </div>
                ) : null}

                {usableNodes.length === 0 ? (
                  <div className="p-4">
                    <EmptyState
                      description={text.nodesEmptyDescription}
                      icon={<Gauge className="h-4 w-4" />}
                      title={text.nodesEmptyTitle}
                    />
                  </div>
                ) : (
                  <section className="table-panel min-w-0 border-0 shadow-none">
                    <div className="min-w-0 overflow-x-auto">
                    <table className="w-full min-w-[720px] table-fixed text-left text-sm">
                      <thead>
                        <tr className="border-b border-line bg-ink-50 text-xs uppercase tracking-wide text-ink-500">
                          <th className="px-4 py-3 w-10"> </th>
                          <th className="px-4 py-3">{text.table.name}</th>
                          <th className="px-4 py-3">{text.table.protocol}</th>
                          <th className="px-4 py-3">{text.table.endpoint}</th>
                          <th className="px-4 py-3">{text.latency}</th>
                          <th className="px-4 py-3">{text.table.tags}</th>
                          <th className="px-4 py-3 text-right">
                            {copy.common.actions}
                          </th>
                        </tr>
                      </thead>
                      <tbody>
                        {usableNodes.map((node) => {
                          const checked = selectedNodeIds.includes(node.id);
                          return (
                            <tr
                              className={`border-b border-line transition ${
                                checked
                                  ? "bg-[rgb(var(--table-row-selected-bg))]"
                                  : "bg-white hover:bg-ink-50/70"
                              }`}
                              key={node.id}
                            >
                              <td className="px-4 py-3">
                                <input
                                  checked={checked}
                                  onChange={() => toggleNode(node.id)}
                                  type="checkbox"
                                />
                              </td>
                              <td className="px-4 py-3">
                                <div className="truncate font-medium text-ink-900" title={node.name}>
                                  {node.name}
                                </div>
                              </td>
                              <td className="px-4 py-3">
                                <span
                                  className={`inline-flex h-6 items-center rounded-md border px-2 text-xs font-medium ${protocolTone(node.protocol)}`}
                                >
                                  {node.protocol}
                                </span>
                              </td>
                              <td className="min-w-0 px-4 py-3 text-ink-700">
                                <span
                                  className="mono-tabular block truncate"
                                  title={`${node.server}:${node.port}`}
                                >
                                  {node.server}:{node.port}
                                </span>
                              </td>
                              <td className="px-4 py-3">
                                <span
                                  className={`inline-flex h-6 items-center rounded-md border px-2 text-xs font-medium ${latencyTone(node.latency_ms)}`}
                                >
                                  {node.latency_ms != null
                                    ? `${node.latency_ms} ms`
                                    : text.latencyUnknown}
                                </span>
                              </td>
                              <td className="px-4 py-3">
                                <div className="flex max-w-48 flex-wrap gap-1">
                                  {(node.tags ?? []).length > 0 ? (
                                    (node.tags ?? []).map((tag) => (
                                      <span
                                        className="rounded-md border border-line bg-ink-50 px-1.5 py-0.5 text-[11px] text-ink-500"
                                        key={`${node.id}-${tag}`}
                                      >
                                        {tag}
                                      </span>
                                    ))
                                  ) : (
                                    <span className="text-ink-400">-</span>
                                  )}
                                </div>
                              </td>
                              <td className="table-action-cell px-4 py-3">
                                <div className="flex justify-end">
                                  <Button
                                    icon={<UploadCloud className="h-4 w-4" />}
                                    onClick={() => setApplyNodeId(node.id)}
                                    size="sm"
                                  >
                                    {text.applyToEnvironment}
                                  </Button>
                                </div>
                              </td>
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                    </div>
                  </section>
                )}
              </div>
            </>
          ) : (
            <div className="flex flex-1 items-center justify-center p-6">
              <EmptyState
                action={
                  <Button
                    icon={<Plus className="h-4 w-4" />}
                    onClick={() => setDraft(defaultDraft())}
                    variant="primary"
                  >
                    {text.addSubscription}
                  </Button>
                }
                description={text.emptyDescription}
                icon={<Link2 className="h-4 w-4" />}
                title={text.emptyTitle}
              />
            </div>
          )}
        </section>
      </div>

      {draft ? (
        <Modal
          footer={
            <>
              <Button onClick={() => setDraft(null)}>{copy.common.cancel}</Button>
              <Button
                disabled={
                  saveMutation.isPending ||
                  !draft.name.trim() ||
                  !draft.url.trim()
                }
                onClick={() => saveMutation.mutate(draft)}
                variant="primary"
              >
                {copy.common.save}
              </Button>
            </>
          }
          open
          title={draft.id ? text.editTitle : text.createTitle}
          widthClass="max-w-2xl"
          onClose={() => setDraft(null)}
        >
          <div className="grid gap-4">
            <TextField
              label={text.fields.name}
              onChange={(event) =>
                setDraft((current) =>
                  current
                    ? {
                        ...current,
                        name: event.target.value,
                      }
                    : current,
                )
              }
              value={draft.name}
            />
            <TextareaField
              hint={text.fields.urlHint}
              label={text.fields.url}
              onChange={(event) =>
                setDraft((current) =>
                  current
                    ? {
                        ...current,
                        url: event.target.value,
                      }
                    : current,
                )
              }
              value={draft.url}
            />
            <TextField
              hint={text.fields.autoSyncInterval}
              label={text.autoSync}
              min={0}
              onChange={(event) =>
                setDraft((current) =>
                  current
                    ? {
                        ...current,
                        auto_sync_interval_minutes:
                          Number(event.target.value) || 0,
                      }
                    : current,
                )
              }
              type="number"
              value={draft.auto_sync_interval_minutes}
            />
            <label className="inline-flex items-center gap-2 text-sm text-ink-700">
              <input
                checked={draft.enabled}
                onChange={(event) =>
                  setDraft((current) =>
                    current
                      ? {
                          ...current,
                          enabled: event.target.checked,
                        }
                      : current,
                  )
                }
                type="checkbox"
              />
              {text.fields.enabled}
            </label>
          </div>
        </Modal>
      ) : null}

      {applyNodeId ? (
        <ApplyNodeModal
          environments={environments}
          loading={applyMutation.isPending}
          node={usableNodes.find((node) => node.id === applyNodeId) ?? null}
          selectedEnvironmentId={selectedEnvironmentId}
          text={text}
          onClose={() => {
            setApplyNodeId(null);
            setSelectedEnvironmentId("");
          }}
          onConfirm={() => {
            if (!selectedEnvironmentId || !applyNodeId) {
              return;
            }
            applyMutation.mutate({
              environment_id: selectedEnvironmentId,
              node_id: applyNodeId,
            });
          }}
          onSelectEnvironment={setSelectedEnvironmentId}
        />
      ) : null}
    </div>
  );
}

function ApplyNodeModal({
  environments,
  loading,
  node,
  selectedEnvironmentId,
  text,
  onClose,
  onConfirm,
  onSelectEnvironment,
}: {
  environments: Environment[];
  loading: boolean;
  node: ProxySubscriptionNode | null;
  selectedEnvironmentId: string;
  text: ReturnType<typeof useI18n>["copy"]["subscriptions"];
  onClose: () => void;
  onConfirm: () => void;
  onSelectEnvironment: (value: string) => void;
}) {
  const { copy } = useI18n();
  return (
    <Modal
      footer={
        <>
          <Button onClick={onClose}>{copy.common.cancel}</Button>
          <Button
            disabled={loading || !selectedEnvironmentId || !node}
            onClick={onConfirm}
            variant="primary"
          >
            {text.applyConfirm}
          </Button>
        </>
      }
      open
      title={text.applyTitle}
      widthClass="max-w-lg"
      onClose={onClose}
    >
      <div className="grid gap-4">
        <div className="rounded-xl border border-line bg-ink-50 px-3.5 py-3">
          <div className="text-sm font-semibold text-ink-900">
            {node?.name ?? "-"}
          </div>
          <div className="mt-1 text-xs text-ink-500">
            {node ? `${node.protocol} · ${node.server}:${node.port}` : ""}
          </div>
        </div>
        <SelectField
          label={text.fields.environment}
          onChange={(event) => onSelectEnvironment(event.target.value)}
          value={selectedEnvironmentId}
        >
          <option value="">{text.fields.environmentPlaceholder}</option>
          {environments.map((environment) => (
            <option key={environment.id} value={environment.id}>
              {environment.name}
            </option>
          ))}
        </SelectField>
        <div className="text-xs leading-5 text-ink-500">{text.applyHint}</div>
      </div>
    </Modal>
  );
}
