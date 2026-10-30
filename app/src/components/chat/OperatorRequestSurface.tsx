import { useRef, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { OcButton, OcMenuItem, OcTextArea } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { operatorPermissionOffer, type OperatorPermissionChoice } from "./operator-permissions";
import { Glyph, type GlyphName } from "../IconGlyphs";
import type { ChatOperatorResponse, ChatRenderMessage } from "./types";

type Props = {
  message: ChatRenderMessage;
  onRespond: (message: ChatRenderMessage, response: ChatOperatorResponse) => Promise<void>;
};

type Question = {
  id: string;
  header: string;
  question: string;
  isOther: boolean;
  isSecret: boolean;
  options: Array<{ label: string; description: string }>;
};

export function OperatorRequestSurface({ message, onRespond }: Props) {
  const data = message.eventData ?? {};
  const method = stringValue(data.method);
  const kind = effectiveRequestKind(message.eventKind, method);
  const resolved = data.resolved === true;
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");

  async function respond(response: ChatOperatorResponse) {
    if (sending || resolved) return;
    setSending(true);
    setError("");
    try {
      await onRespond(message, response);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setSending(false);
    }
  }

  const presentation = requestPresentation(kind, data, message.text);
  return (
    <section className={`operator-request-surface is-${kind}${resolved ? " is-resolved" : ""}`}>
      <div className="operator-request-heading">
        <span className="operator-request-glyph"><Glyph name={presentation.glyph} size={15} /></span>
        <div className="min-w-0 flex-1">
          <div className="operator-request-kicker">{presentation.kicker}</div>
          <div className="operator-request-title">{presentation.title}</div>
        </div>
        {resolved ? (
          <span className="operator-request-status"><Glyph name="check" size={12} /> Answered</span>
        ) : sending ? (
          <span className="operator-request-status"><span className="composer-spinner size-3" /> Sending</span>
        ) : null}
      </div>

      {presentation.detail ? <div className="operator-request-detail">{presentation.detail}</div> : null}
      {presentation.context.length ? (
        <dl className="operator-request-context">
          {presentation.context.map((entry) => (
            <div key={entry.label}>
              <dt>{entry.label}</dt>
              <dd>{entry.code ? <code>{entry.value}</code> : entry.value}</dd>
            </div>
          ))}
        </dl>
      ) : null}

      {!resolved && kind === "approval" ? (
        <ApprovalResponse data={data} clientInterface={message.interfaceId} disabled={sending} onRespond={(decision) => void respond({ kind: "approval", decision })} />
      ) : null}
      {!resolved && kind === "user_input" ? (
        <QuestionResponse data={data} disabled={sending} onRespond={(answers) => void respond({ kind: "user_input", answers })} />
      ) : null}
      {!resolved && kind === "elicitation" ? (
        <ElicitationResponse data={data} disabled={sending} onRespond={(answer) => void respond({ kind: "elicitation", answer })} />
      ) : null}
      {resolved ? <ResolvedResponse response={data.response} /> : null}
      <AnimatePresence initial={false}>
        {error ? (
          <motion.div animate={{ height: "auto", opacity: 1 }} className="operator-request-error" exit={{ height: 0, opacity: 0 }} initial={{ height: 0, opacity: 0 }}>
            <Glyph name="warning" size={13} />
            <span>{error}</span>
          </motion.div>
        ) : null}
      </AnimatePresence>
    </section>
  );
}

function ApprovalResponse({ data, clientInterface, disabled, onRespond }: {
  data: Record<string, unknown>;
  clientInterface?: string;
  disabled: boolean;
  onRespond: (decision: "accept" | "acceptForSession" | "decline" | "cancel" | Record<string, unknown>) => void;
}) {
  const requestKind = stringValue(data.kind);
  const detail = recordValue(data.detail);
  if (["application", "browser_file_upload", "browser_history_read"].includes(requestKind)) {
    const application = recordValue(detail.application);
    const policy = recordValue(detail.native_policy);
    const complete = requestKind === "application"
      ? Boolean(stringValue(application.display_name) && stringValue(application.bundle_path)
        && stringValue(application.bundle_id) && stringValue(data.call_id))
        && detail.approval_scope === "formed_app_target"
        && (application.instance === null || Object.keys(recordValue(application.instance)).length > 0)
      : requestKind === "browser_file_upload" ? browserUploadPresentation(data).complete : browserHistoryPresentation(data).complete;
    const help = requestKind === "application"
      ? application.instance === null ? "Allow opening and accessing this application." : "Allow accessing this running application."
      : requestKind === "browser_file_upload" ? "Allow the listed files to be supplied to this page."
      : "Allow this request to read the selected browsing history.";
    return <ToolPermissionResponse data={data} clientInterface={clientInterface} complete={complete} disabled={disabled}
      help={help} warning={requestKind === "application" ? stringValue(policy.warning_subtitle) : ""} onRespond={onRespond} />;
  }
  if (requestKind === "permission" || stringValue(data.method) === "item/permissions/requestApproval") {
    const requested = recordValue(detail.permissions);
    return (
      <div className="operator-request-actions is-stacked">
        <div className="operator-request-help">Grant exactly the projected permission request for this turn or for the current provider session.</div>
        <div className="flex flex-wrap gap-2">
          <OcButton disabled={disabled} onClick={() => onRespond(permissionDecision(requested, "turn"))} size="sm" variant="primary">Allow for this turn</OcButton>
          <OcButton disabled={disabled} onClick={() => onRespond(permissionDecision(requested, "session"))} size="sm" variant="secondary">Allow for session</OcButton>
        </div>
      </div>
    );
  }
  return (
    <div className="operator-request-actions">
      <OcButton disabled={disabled} onClick={() => onRespond("accept")} size="sm" variant="primary">Allow</OcButton>
      <OcButton disabled={disabled} onClick={() => onRespond("acceptForSession")} size="sm" variant="secondary">Allow for session</OcButton>
      <OcButton disabled={disabled} onClick={() => onRespond("decline")} size="sm" variant="secondary">Decline</OcButton>
    </div>
  );
}

function ToolPermissionResponse({ data, clientInterface, complete, disabled, help, warning, onRespond }: {
  data: Record<string, unknown>;
  clientInterface?: string;
  complete: boolean;
  disabled: boolean;
  help: string;
  warning: string;
  onRespond: (decision: "accept" | "decline" | "cancel" | Record<string, unknown>) => void;
}) {
  const [open, setOpen] = useState(false);
  const anchorRef = useRef<HTMLButtonElement>(null);
  let offer: ReturnType<typeof operatorPermissionOffer> = null;
  let failure = complete ? "" : "The permission request is incomplete.";
  try { offer = operatorPermissionOffer(data, clientInterface); }
  catch (error) { failure = error instanceof Error ? error.message : String(error); }
  const choices = offer?.offered_choices ?? [];
  const primary = choices.find((choice) => choice.decision === "accept"
    && choice.persistence === (data.kind === "application" ? "session" : "once"))
    ?? choices.find((choice) => choice.decision === "accept");
  const decline = choices.find((choice) => choice.decision === "decline" && choice.persistence === "once");
  const remaining = choices.filter((choice) => choice !== primary && choice !== decline);
  const blocked = disabled || Boolean(failure);
  const label = (choice: OperatorPermissionChoice) => choice.decision === "accept"
    ? choice.persistence === "once" ? "Allow once" : choice.persistence === "session" ? "Allow for this task" : "Always allow"
    : choice.persistence === "once" ? "Decline" : choice.persistence === "session" ? "Decline for this task" : "Always decline";
  const choose = (choice: OperatorPermissionChoice) => {
    setOpen(false);
    onRespond({ decision: choice.decision, selected_persistence: choice.persistence });
  };
  return (
    <div className="operator-request-actions is-stacked">
      <div className="operator-request-help">{help}</div>
      {warning ? <div className="operator-request-help">{warning}</div> : null}
      <div className="flex flex-wrap gap-2">
        {!offer && !failure ? <>
          <OcButton disabled={blocked} onClick={() => onRespond("accept")} size="sm" variant="primary">Allow once</OcButton>
          <OcButton disabled={blocked} onClick={() => onRespond("decline")} size="sm" variant="secondary">Decline</OcButton>
        </> : <>
          {primary ? <OcButton disabled={blocked} onClick={() => choose(primary)} size="sm" variant="primary">{label(primary)}</OcButton> : null}
          {remaining.length ? <>
            <OcButton aria-label="More permission choices" buttonRef={anchorRef} disabled={blocked} onClick={() => setOpen(!open)} size="sm" variant="secondary" leadingIcon="chevronDown" />
            <OcDropdown anchorRef={anchorRef} open={open && !blocked} onOpenChange={setOpen} aria-label="Permission choices">
              {remaining.map((choice) => <OcMenuItem key={`${choice.decision}:${choice.persistence}`} label={label(choice)} onClick={() => choose(choice)} />)}
            </OcDropdown>
          </> : null}
          {decline ? <OcButton disabled={blocked} onClick={() => choose(decline)} size="sm" variant="secondary">{label(decline)}</OcButton> : null}
        </>}
        <OcButton disabled={blocked} onClick={() => onRespond(offer ? { decision: "cancel" } : "cancel")} size="sm" variant="ghost">Cancel</OcButton>
      </div>
      {failure ? <div className="operator-request-error">{failure}</div> : null}
    </div>
  );
}

function QuestionResponse({ data, disabled, onRespond }: {
  data: Record<string, unknown>;
  disabled: boolean;
  onRespond: (answers: Record<string, string[]>) => void;
}) {
  const questions = normalizeQuestions(recordValue(data.requested_schema).questions ?? data.questions);
  const [answers, setAnswers] = useState<Record<string, string>>(() => Object.fromEntries(questions.map((question) => [question.id, ""])));
  const [other, setOther] = useState<Record<string, string>>({});
  const complete = questions.length > 0 && questions.every((question) => {
    const answer = answers[question.id] ?? "";
    return answer === "__other__" ? Boolean(other[question.id]?.trim()) : Boolean(answer.trim());
  });

  if (!questions.length) {
    return <FreeformUserInput disabled={disabled} onRespond={(value) => onRespond({ answer: [value] })} />;
  }
  return (
    <form className="operator-question-form" onSubmit={(event) => {
      event.preventDefault();
      if (!complete) return;
      onRespond(Object.fromEntries(questions.map((question) => {
        const answer = answers[question.id] === "__other__" ? other[question.id]?.trim() ?? "" : answers[question.id] ?? "";
        return [question.id, [answer]];
      })));
    }}>
      {questions.map((question, questionIndex) => (
        <fieldset className="operator-question" disabled={disabled} key={question.id}>
          <legend>
            <span>{question.header || `Question ${questionIndex + 1}`}</span>
            <strong>{question.question}</strong>
          </legend>
          {question.options.length ? (
            <div className="operator-question-options">
              {question.options.map((option) => (
                <label className={`operator-question-option${answers[question.id] === option.label ? " is-selected" : ""}`} key={option.label}>
                  <input checked={answers[question.id] === option.label} name={question.id} onChange={() => setAnswers((current) => ({ ...current, [question.id]: option.label }))} type="radio" />
                  <span><strong>{option.label}</strong>{option.description ? <small>{option.description}</small> : null}</span>
                </label>
              ))}
              {question.isOther ? (
                <label className={`operator-question-option${answers[question.id] === "__other__" ? " is-selected" : ""}`}>
                  <input checked={answers[question.id] === "__other__"} name={question.id} onChange={() => setAnswers((current) => ({ ...current, [question.id]: "__other__" }))} type="radio" />
                  <span className="min-w-0 flex-1"><strong>Other</strong></span>
                </label>
              ) : null}
              {answers[question.id] === "__other__" ? (
                <input autoFocus className="operator-request-input" onChange={(event) => setOther((current) => ({ ...current, [question.id]: event.target.value }))} placeholder="Type another answer" type={question.isSecret ? "password" : "text"} value={other[question.id] ?? ""} />
              ) : null}
            </div>
          ) : (
            <OcTextArea label={question.question || "Answer"} onChange={(event) => setAnswers((current) => ({ ...current, [question.id]: event.target.value }))} placeholder="Enter your answer" textareaClassName="operator-request-textarea" value={answers[question.id] ?? ""} />
          )}
        </fieldset>
      ))}
      <div className="operator-request-actions">
        <OcButton disabled={disabled || !complete} size="sm" type="submit" variant="primary">Submit answer</OcButton>
        {numberValue(recordValue(data.requested_schema).autoResolutionMs ?? data.autoResolutionMs ?? data.auto_resolution_ms) > 0 ? (
          <span className="operator-request-auto">May resolve automatically</span>
        ) : null}
      </div>
    </form>
  );
}

function ElicitationResponse({ data, disabled, onRespond }: {
  data: Record<string, unknown>;
  disabled: boolean;
  onRespond: (answer: unknown) => void;
}) {
  const schema = recordValue(data.requested_schema);
  const properties = recordValue(schema.properties);
  const required = new Set(arrayValue(schema.required).map(stringValue).filter(Boolean));
  const [values, setValues] = useState<Record<string, unknown>>(() => initialSchemaValues(properties));
  const [structuredValidity, setStructuredValidity] = useState<Record<string, boolean>>({});
  const [freeform, setFreeform] = useState("");
  const fields = Object.entries(properties);
  const valid = fields.every(([name]) => !required.has(name) || valuePresent(values[name]))
    && Object.values(structuredValidity).every(Boolean);

  if (!fields.length) {
    return (
      <form className="operator-request-form" onSubmit={(event) => {
        event.preventDefault();
        if (!freeform.trim()) return;
        onRespond({ action: "accept", content: freeform.trim(), _meta: null });
      }}>
        <OcTextArea label="Response" disabled={disabled} onChange={(event) => setFreeform(event.target.value)} placeholder="Enter your response" textareaClassName="operator-request-textarea" value={freeform} />
        <div className="operator-request-actions">
          <OcButton disabled={disabled || !freeform.trim()} size="sm" type="submit" variant="primary">Submit</OcButton>
          <OcButton disabled={disabled} onClick={() => onRespond({ action: "decline", _meta: null })} size="sm" variant="secondary">Decline</OcButton>
        </div>
      </form>
    );
  }
  return (
    <form className="operator-request-form" onSubmit={(event) => {
      event.preventDefault();
      if (!valid) return;
      onRespond({ action: "accept", content: values, _meta: null });
    }}>
      {fields.map(([name, descriptor]) => (
        <SchemaControl
          descriptor={recordValue(descriptor)}
          disabled={disabled}
          key={name}
          name={name}
          required={required.has(name)}
          value={values[name]}
          onChange={(value) => setValues((current) => ({ ...current, [name]: value }))}
          onValidityChange={(fieldValid) => setStructuredValidity((current) => current[name] === fieldValid ? current : { ...current, [name]: fieldValid })}
        />
      ))}
      <div className="operator-request-actions">
        <OcButton disabled={disabled || !valid} size="sm" type="submit" variant="primary">Submit</OcButton>
        <OcButton disabled={disabled} onClick={() => onRespond({ action: "decline", _meta: null })} size="sm" variant="secondary">Decline</OcButton>
      </div>
    </form>
  );
}

function FreeformUserInput({ disabled, onRespond }: { disabled: boolean; onRespond: (value: string) => void }) {
  const [value, setValue] = useState("");
  return (
    <form className="operator-request-form" onSubmit={(event) => {
      event.preventDefault();
      if (value.trim()) onRespond(value.trim());
    }}>
      <OcTextArea label="Answer" disabled={disabled} onChange={(event) => setValue(event.target.value)} placeholder="Enter your answer" textareaClassName="operator-request-textarea" value={value} />
      <div className="operator-request-actions"><OcButton disabled={disabled || !value.trim()} size="sm" type="submit" variant="primary">Submit answer</OcButton></div>
    </form>
  );
}

function SchemaControl({ descriptor, disabled, name, required, value, onChange, onValidityChange }: {
  descriptor: Record<string, unknown>;
  disabled: boolean;
  name: string;
  required: boolean;
  value: unknown;
  onChange: (value: unknown) => void;
  onValidityChange: (valid: boolean) => void;
}) {
  const options = arrayValue(descriptor.enum);
  const type = stringValue(descriptor.type) || "string";
  const title = stringValue(descriptor.title) || name;
  const description = stringValue(descriptor.description);
  const [raw, setRaw] = useState(() => type === "object" || type === "array" ? JSON.stringify(value, null, 2) : "");
  const [invalid, setInvalid] = useState(false);
  const id = `operator-schema-${name.replace(/[^a-z0-9_-]/gi, "-")}`;
  const labelId = `${id}-label`;

  function updateStructured(nextRaw: string) {
    setRaw(nextRaw);
    try {
      const next = JSON.parse(nextRaw);
      const valid = type === "array" ? Array.isArray(next) : Boolean(next) && typeof next === "object" && !Array.isArray(next);
      setInvalid(!valid);
      onValidityChange(valid);
      if (valid) onChange(next);
    } catch {
      setInvalid(true);
      onValidityChange(false);
    }
  }

  return (
    <div className="operator-schema-field">
      <span id={labelId}>{title}{required ? <b aria-label="required"> *</b> : null}</span>
      {description ? <small>{description}</small> : null}
      {options.length ? (
        <select aria-labelledby={labelId} disabled={disabled} id={id} onChange={(event) => onChange(parseSchemaValue(event.target.value, type))} value={String(value ?? "")}>
          {!required ? <option value="">Select…</option> : null}
          {options.map((option) => <option key={String(option)} value={String(option)}>{String(option)}</option>)}
        </select>
      ) : type === "boolean" ? (
        <span className="operator-schema-checkbox"><input aria-labelledby={labelId} checked={value === true} disabled={disabled} id={id} onChange={(event) => onChange(event.target.checked)} type="checkbox" /> Enable</span>
      ) : type === "object" || type === "array" ? (
        <OcTextArea error={invalid ? `Enter valid ${type === "array" ? "JSON array" : "JSON object"}.` : undefined} label={title} disabled={disabled} id={id} onChange={(event) => updateStructured(event.target.value)} textareaClassName="operator-request-textarea is-code" value={raw} />
      ) : (
        <input aria-labelledby={labelId} disabled={disabled} id={id} onChange={(event) => onChange(parseSchemaValue(event.target.value, type))} type={type === "number" || type === "integer" ? "number" : descriptor.format === "password" ? "password" : "text"} value={String(value ?? "")} />
      )}
    </div>
  );
}

function ResolvedResponse({ response }: { response: unknown }) {
  const summary = resolvedSummary(response);
  if (!summary) return null;
  return <div className="operator-request-resolved"><Glyph name="check" size={13} /><span>{summary}</span></div>;
}

export function requestPresentation(kind: "approval" | "elicitation" | "user_input", data: Record<string, unknown>, fallback: string): {
  glyph: GlyphName;
  kicker: string;
  title: string;
  detail: string;
  context: Array<{ label: string; value: string; code?: boolean }>;
} {
  const detail = recordValue(data.detail);
  const requestKind = stringValue(data.kind);
  const context: Array<{ label: string; value: string; code?: boolean }> = [];
  if (kind === "approval" && requestKind === "browser_history_read") {
    const history = browserHistoryPresentation(data);
    if (history.source) context.push({ label: "History source", value: history.source, code: true });
    if (history.queries) context.push({ label: "Queries", value: JSON.stringify(history.queries), code: true });
    if (history.from) context.push({ label: "From", value: history.from });
    if (history.to) context.push({ label: "To", value: history.to });
    if (history.limit !== undefined) context.push({ label: "Result limit", value: String(history.limit) });
    return { glyph: "lock", kicker: "Browsing history", title: "Allow browsing history access?", detail: "", context };
  }
  if (kind === "approval" && requestKind === "application") {
    const application = recordValue(detail.application);
    const name = stringValue(application.display_name);
    const path = stringValue(application.bundle_path);
    const bundleId = stringValue(application.bundle_id);
    if (path) context.push({ label: "Application", value: path, code: true });
    if (bundleId) context.push({ label: "Bundle ID", value: bundleId, code: true });
    return { glyph: "lock", kicker: "Application access", title: name ? `Allow access to ${name}?` : "Application approval requested", detail: "", context };
  }
  if (kind === "approval" && requestKind === "browser_file_upload") {
    const upload = browserUploadPresentation(data);
    if (upload.origin) context.push({ label: "Page origin", value: upload.origin, code: true });
    upload.files.forEach((file, index) => context.push({
      label: `File ${index + 1}`,
      value: `${JSON.stringify(file.filename)} · ${file.sizeBytes === null ? "Size unavailable" : `${file.sizeBytes.toLocaleString("en-US")} bytes`}`,
      code: true,
    }));
    return { glyph: "lock", kicker: "File upload", title: "Allow file upload?", detail: "", context };
  }
  const command = commandValue(detail.command);
  const cwd = stringValue(detail.cwd);
  const reason = stringValue(detail.reason);
  if (command) context.push({ label: "Command", value: command, code: true });
  if (cwd) context.push({ label: "Working directory", value: cwd, code: true });
  const changes = recordValue(detail.fileChanges);
  const changeCount = Object.keys(changes).length;
  if (changeCount) context.push({ label: "Files", value: `${changeCount} ${changeCount === 1 ? "change" : "changes"}` });
  if (reason) context.push({ label: "Reason", value: reason });
  if (kind === "approval") {
    return {
      glyph: requestKind === "exec" ? "terminal" : requestKind === "patch" ? "changes" : "lock",
      kicker: requestKind === "exec" ? "Command approval" : requestKind === "patch" ? "File change approval" : "Permission approval",
      title: stringValue(data.title) || fallback || "Approval requested",
      detail: requestKind === "permission" ? permissionSummary(recordValue(detail.permissions)) : "",
      context,
    };
  }
  return {
    glyph: "hand",
    kicker: kind === "user_input" ? "Question from the model" : "Input requested",
    title: stringValue(data.message) || fallback || "The run is waiting for your response",
    detail: "",
    context,
  };
}

function browserHistoryPresentation(data: Record<string, unknown>) {
  const detail = recordValue(data.detail);
  const history = recordValue(detail.history);
  const source = recordValue(history.source);
  const query = recordValue(history.query);
  const queries = Array.isArray(query.queries) && query.queries.every((value) => typeof value === "string")
    ? query.queries as string[] : undefined;
  const from = typeof query.from === "string" ? query.from : undefined;
  const to = typeof query.to === "string" ? query.to : undefined;
  const limit = typeof query.limit === "number" && Number.isSafeInteger(query.limit) && query.limit > 0 ? query.limit : undefined;
  return {
    source: stringValue(source.id), queries, from, to, limit,
    complete: detail.approval_scope === "browser_history_read" && Boolean(stringValue(data.request_id) && stringValue(data.call_id) && stringValue(source.id))
      && Object.keys(source).length === 2 && Array.isArray(source.content) && source.content.length === 32
      && source.content.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)
      && typeof history.source_payload_digest === "string" && /^[a-f0-9]{64}$/.test(history.source_payload_digest)
      && typeof history.source_epoch === "string" && /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(history.source_epoch)
      && Object.keys(query).every((key) => ["queries", "from", "to", "limit"].includes(key))
      && (!Object.hasOwn(query, "queries") || Boolean(queries?.length))
      && (!Object.hasOwn(query, "from") || from !== undefined) && (!Object.hasOwn(query, "to") || to !== undefined)
      && (!Object.hasOwn(query, "limit") || limit !== undefined),
  };
}

function browserUploadPresentation(data: Record<string, unknown>): {
  origin: string;
  files: Array<{ filename: string; sizeBytes: number | null }>;
  complete: boolean;
} {
  const detail = recordValue(data.detail);
  const upload = recordValue(detail.upload);
  const origin = typeof upload.top_level_origin === "string" ? upload.top_level_origin : "";
  let validOrigin = false;
  try {
    const parsed = new URL(origin);
    validOrigin = (parsed.protocol === "http:" || parsed.protocol === "https:") && parsed.origin === origin;
  } catch {
    validOrigin = false;
  }
  const files = arrayValue(upload.files).map((value) => {
    const file = recordValue(value);
    return {
      filename: typeof file.filename === "string" ? file.filename : "",
      sizeBytes: typeof file.size_bytes === "number" && Number.isSafeInteger(file.size_bytes) && file.size_bytes >= 0 ? file.size_bytes : null,
    };
  });
  return {
    origin,
    files,
    complete: detail.approval_scope === "browser_file_upload" && validOrigin
      && Boolean(stringValue(data.request_id) && stringValue(data.call_id))
      && files.length > 0 && files.every((file) => file.filename.length > 0 && file.sizeBytes !== null),
  };
}

function effectiveRequestKind(kind: ChatRenderMessage["eventKind"], method: string): "approval" | "elicitation" | "user_input" {
  if (kind === "approval") return "approval";
  if (kind === "user_input" || method === "item/tool/requestUserInput") return "user_input";
  return "elicitation";
}

function permissionDecision(requested: Record<string, unknown>, scope: "turn" | "session"): Record<string, unknown> {
  const permissions: Record<string, unknown> = {};
  if (requested.network != null) permissions.network = requested.network;
  if (requested.fileSystem != null) permissions.fileSystem = requested.fileSystem;
  return { permissions, scope };
}

function permissionSummary(requested: Record<string, unknown>): string {
  const capabilities = [];
  if (requested.network != null) capabilities.push("additional network access");
  if (requested.fileSystem != null) capabilities.push("additional filesystem access");
  return capabilities.length ? `The provider requested ${capabilities.join(" and ")}.` : "The provider requested additional permissions.";
}

function normalizeQuestions(value: unknown): Question[] {
  return arrayValue(value).flatMap((entry, index) => {
    const question = recordValue(entry);
    const id = stringValue(question.id) || `question_${index + 1}`;
    const prompt = stringValue(question.question);
    if (!prompt) return [];
    const options = arrayValue(question.options).flatMap((candidate) => {
      const option = recordValue(candidate);
      const label = stringValue(option.label);
      return label ? [{ label, description: stringValue(option.description) }] : [];
    });
    return [{
      id,
      header: stringValue(question.header),
      question: prompt,
      isOther: question.isOther === true || question.is_other === true,
      isSecret: question.isSecret === true || question.is_secret === true,
      options,
    }];
  });
}

function initialSchemaValues(properties: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(properties).map(([name, value]) => {
    const descriptor = recordValue(value);
    const options = arrayValue(descriptor.enum);
    const type = stringValue(descriptor.type);
    const initial = descriptor.default ?? options[0] ?? (type === "boolean" ? false : type === "object" ? {} : type === "array" ? [] : "");
    return [name, initial];
  }));
}

function parseSchemaValue(value: string, type: string): unknown {
  if (type === "number" || type === "integer") {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : value;
  }
  return value;
}

function valuePresent(value: unknown): boolean {
  if (typeof value === "string") return Boolean(value.trim());
  if (Array.isArray(value)) return value.length > 0;
  return value !== null && value !== undefined;
}

function resolvedSummary(value: unknown): string {
  const response = recordValue(value);
  const approval = recordValue(response.operator_approval_response);
  const elicitation = recordValue(response.operator_elicitation_response);
  const userInput = recordValue(response.operator_user_input_response);
  const decision = response.decision ?? approval.decision;
  if (typeof decision === "string") {
    const scope = response.selected_persistence ?? approval.selected_persistence;
    const suffix = scope === "session" ? " for this task" : scope === "always" ? " always" : scope === "once" ? " once" : "";
    return decision === "accept" ? `Allowed${suffix}` : decision === "acceptForSession" ? "Allowed for session"
      : decision === "decline" ? `Declined${suffix}` : decision === "cancel" ? "Cancelled" : decision;
  }
  if (decision && typeof decision === "object") return "Permission granted";
  if (response.answer !== undefined || response.answers !== undefined || elicitation.answer !== undefined || userInput.answers !== undefined) return "Response submitted";
  return "";
}

function commandValue(value: unknown): string {
  if (typeof value === "string") return value.trim();
  return arrayValue(value).map(stringValue).filter(Boolean).join(" ");
}

function recordValue(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function arrayValue(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function numberValue(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}
