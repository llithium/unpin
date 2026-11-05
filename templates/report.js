      (() => {
        const groups = Array.from(document.querySelectorAll("[data-group]"));
        const links = Array.from(
          document.querySelectorAll(".match-link[data-target]"),
        );
        const linkByTarget = new Map(
          links.map((link) => [link.dataset.target, link]),
        );
        const scopeFilters = Array.from(
          document.querySelectorAll('input[name="scope-filter"]'),
        );
        const kindFilters = Array.from(
          document.querySelectorAll('input[name="kind-filter"]'),
        );
        const quickWins = document.querySelector("#quick-wins");
        const unreviewedOnly = document.querySelector("#unreviewed-only");
        const visibleCount = document.querySelector("#visible-count");
        const previous = document.querySelector("#previous-match");
        const next = document.querySelector("#next-match");
        const overview = document.querySelector("#overview-toggle");
        const empty = document.querySelector("#filter-empty");
        const progress = document.querySelector("#review-progress");
        const progressFill = document.querySelector("#progress-fill");
        const progressBlock = document.querySelector(".progress-block");
        const resetReview = document.querySelector("#reset-review");
        const status = document.querySelector("#interaction-status");
        const storageKey = "unpin:review:v3";
        const viewStorageKey = "unpin:view:v1";
        let decisions = new Map();
        const dirtyDecisions = new Set();
        let storedRevision = null;
        const actions = new Set(["reviewed", "keep_all", "remove", "later", "not_duplicate"]);
        const groupByKey = new Map(groups.map((group) => [group.dataset.reviewKey, group]));
        const isQuickWin = (group) => group.dataset.scope === "same-board" &&
          group.dataset.kind === "exact" && group.dataset.previewOnly !== "true";
        const isComplete = (decision) => Boolean(decision?.reviewed);
        const validKey = (key) => typeof key === "string" && /^(exact|visual):(same-board|cross-board):[a-f0-9]{64}$/.test(key);
        const parseDecisions = (value) => {
          if (!value || value.schema_version !== 1 || !Array.isArray(value.decisions) || value.decisions.length > 100000) {
            throw new Error("Unsupported decisions file; expected schema version 1.");
          }
          const parsed = new Map();
          for (const entry of value.decisions) {
            if (!entry || !validKey(entry.key) || !actions.has(entry.action) || parsed.has(entry.key) ||
                typeof entry.reviewed !== "boolean" || (entry.action === "later" && entry.reviewed) ||
                (entry.action !== "later" && entry.action !== "remove" && !entry.reviewed) ||
                !Array.isArray(entry.items) || !entry.items.length ||
                !entry.items.every((item) => item && typeof item.pin_id === "string" && item.pin_id.length > 0 &&
                  typeof item.pin_url === "string" && typeof item.image_url === "string" &&
                  typeof item.image_sha256 === "string" && typeof item.media_kind === "string") ||
                !Array.isArray(entry.remove_pin_ids) ||
                !entry.remove_pin_ids.every((id) => typeof id === "string") ||
                new Set(entry.items.map((item) => item.pin_id)).size !== entry.items.length ||
                new Set(entry.remove_pin_ids).size !== entry.remove_pin_ids.length ||
                (entry.action === "remove" && !entry.remove_pin_ids.length) ||
                (entry.action !== "remove" && entry.remove_pin_ids.length)) {
              throw new Error("Invalid review decision; no decisions were imported.");
            }
            const itemById = new Map(entry.items.map((item) => [item.pin_id, item]));
            if (entry.remove_pin_ids.some((id) => !itemById.has(id))) {
              throw new Error("Invalid review decision; no decisions were imported.");
            }
            const group = groupByKey.get(entry.key);
            if (group) {
              const expected = decisionRecord(group, "reviewed", [], true).items;
              if (expected.length !== entry.items.length || expected.some((item) => {
                  const candidate = itemById.get(item.pin_id);
                  return !candidate || item.image_sha256 !== candidate.image_sha256 ||
                    item.media_kind !== candidate.media_kind || item.image_url !== candidate.image_url;
                })) {
                throw new Error("A decision's image content or pin membership does not match this report.");
              }
            }
            // Retain only known fields. Imported links are data, never inserted into the page.
            parsed.set(entry.key, { key: entry.key, action: entry.action, reviewed: entry.reviewed,
              remove_pin_ids: [...entry.remove_pin_ids], items: entry.items.map((item) => ({
                pin_id: item.pin_id, pin_url: item.pin_url, image_url: item.image_url,
                image_sha256: item.image_sha256, media_kind: item.media_kind,
              })) });
          }
          return parsed;
        };
        const decisionRecord = (group, action, removePinIds, reviewed) => ({
          key: group.dataset.reviewKey, action, reviewed, remove_pin_ids: removePinIds,
          items: [...group.querySelectorAll("[data-pin-id]")].map((item) => ({
            pin_id: item.dataset.pinId,
            pin_url: item.querySelector(".button.primary, a[data-image-url]")?.href || "",
            image_url: item.querySelector(".image-stage")?.href || item.querySelector("a[data-image-url]")?.dataset.imageUrl || "",
            image_sha256: item.dataset.imageSha256, media_kind: item.dataset.mediaKind,
          })),
        });
        const currentGroupKeys = groups
          .map((group) => group.dataset.reviewKey)
          .filter((key) => typeof key === "string" && key.length > 0);
        let active = groups[0] || null;
        let quickOnly = false;

        let visible = groups;
        let visibleIndices = new Map(visible.map((group, index) => [group, index]));
        const visibleGroups = () => visible;
        const reviewedKeys = new Set();
        let quickWinCount = groups.filter(isQuickWin).length;

        const announce = (message) => {
          if (status) status.textContent = message;
        };

        const readStoredState = () => {
          try {
            const value = window.localStorage.getItem(storageKey);
            storedRevision = value;
            return value ? JSON.parse(value) : null;
          } catch (_) {
            return null;
          }
        };

        const mergeStoredDecisions = () => {
          const value = window.localStorage.getItem(storageKey);
          if (value === storedRevision) return false;
          const latest = value ? parseDecisions(JSON.parse(value)) : new Map();
          dirtyDecisions.forEach((key) => {
            if (decisions.has(key)) latest.set(key, decisions.get(key));
            else latest.delete(key);
          });
          const changed = groups.some((group) => JSON.stringify(decisions.get(group.dataset.reviewKey)) !== JSON.stringify(latest.get(group.dataset.reviewKey)));
          decisions = latest;
          storedRevision = value;
          return changed;
        };
        const writeStoredState = () => {
          try {
            if (dirtyDecisions.size) {
              // Merge only this page's edits with other open reports. History
              // is persisted on edits, never on ordinary match navigation.
              let changed = false;
              try { changed = mergeStoredDecisions(); } catch (_) { /* Preserve valid session decisions if storage is malformed. */ }
              const serialized = JSON.stringify({ schema_version: 1, decisions: [...decisions.values()] });
              window.localStorage.setItem(storageKey, serialized);
              storedRevision = serialized;
              dirtyDecisions.clear();
              if (changed) { groups.forEach(syncDecisionUi); applyFilter(); updateProgress(); }
            } else {
              window.localStorage.setItem(viewStorageKey, JSON.stringify({
                quickOnly, scope: scopeFilters.find((filter) => filter.checked)?.value,
                kind: kindFilters.find((filter) => filter.checked)?.value,
                unreviewedOnly: Boolean(unreviewedOnly?.checked),
                overview: document.body.classList.contains("overview-mode"),
                active: active?.dataset.reviewKey || null,
              }));
            }
          } catch (_) { document.querySelector("#storage-notice").hidden = false; }
        };

        const markImageReady = (image, decode = false) => {
          const reveal = () => image.classList.add("is-loaded");
          const fail = () => {
            image.closest(".image-stage")?.classList.add("is-error");
            reveal();
          };
          if (image.complete && image.naturalWidth === 0) {
            fail();
            return;
          }
          if (image.complete && image.naturalWidth > 0) {
            reveal();
            return;
          }
          image.addEventListener("load", reveal, { once: true });
          image.addEventListener("error", fail, { once: true });
          if (decode && typeof image.decode === "function") {
            image
              .decode()
              .then(reveal)
              .catch(() => {});
          }
        };

        const warmGroupImages = (group) => {
          group?.querySelectorAll(".image-stage img").forEach((image) => {
            image.loading = "eager";
            markImageReady(image, true);
          });
        };

        const initializeImages = () => {
          groups.forEach((group) => {
            group
              .querySelectorAll(".image-stage img")
              .forEach((image) => markImageReady(image));
          });
        };

        const warmNearbyImages = () => {
          const visible = visibleGroups();
          const index = visibleIndices.get(active) ?? -1;
          if (index < 0) return;
          warmGroupImages(visible[index - 1]);
          warmGroupImages(visible[index]);
          warmGroupImages(visible[index + 1]);
        };

        const updateProgress = () => {
          const reviewed = reviewedKeys.size;
          if (progress) progress.textContent = `${reviewed} / ${groups.length}`;
          if (progressFill) {
            const completion = groups.length ? reviewed / groups.length : 0;
            progressFill.style.transform = `scaleX(${completion})`;
          }
          if (progressBlock) {
            progressBlock.setAttribute(
              "aria-label",
              `Review progress: ${reviewed} of ${groups.length} reviewed`,
            );
          }
          if (resetReview) resetReview.disabled = !groups.some((group) => decisions.has(group.dataset.reviewKey));
        };

        const acknowledgeReview = () => {
          if (!progressBlock) return;
          progressBlock.classList.remove("is-updated");
          requestAnimationFrame(() => {
            progressBlock.classList.add("is-updated");
          });
        };

        const updateControls = () => {
          const visible = visibleGroups();
          const index = visibleIndices.get(active) ?? -1;
          if (previous) previous.disabled = index <= 0;
          if (next) next.disabled = index < 0 || index >= visible.length - 1;
          const mobileCount = document.querySelector("[data-mobile-count]");
          if (mobileCount) mobileCount.textContent = index < 0 ? "0 matches" : `Match ${index + 1} of ${visible.length}`;
        };

        const syncQuickWins = () => {
          if (!quickWins) return;
          const scope = scopeFilters.find((filter) => filter.checked)?.value;
          const kind = kindFilters.find((filter) => filter.checked)?.value;
          const quickWinCountLabel = quickWins.querySelector(".filter-count");
          if (quickWinCountLabel) {
            quickWinCountLabel.textContent = quickWinCount;
          }
          quickWins
            .closest(".filter-controls-meta")
            ?.toggleAttribute("hidden", quickWinCount === 0);
          const isQuickWins = quickOnly &&
            (!scopeFilters.length || scope === "same-board") &&
            (!kindFilters.length || kind === "exact") &&
            (!unreviewedOnly || unreviewedOnly.checked);
          quickWins.setAttribute(
            "aria-pressed",
            isQuickWins ? "true" : "false",
          );
        };

        const setActive = (group, options = {}) => {
          if (!group || group.classList.contains("is-filtered")) return;
          warmGroupImages(group);
          active?.classList.remove("is-active");
          linkByTarget.get(active?.id)?.setAttribute("aria-current", "false");
          active = group;
          active.classList.add("is-active");
          const activeLink = linkByTarget.get(active.id);
          activeLink?.setAttribute("aria-current", "true");
          const nav = activeLink?.closest(".match-nav");
          if (nav) {
            const linkRect = activeLink.getBoundingClientRect();
            const navRect = nav.getBoundingClientRect();
            if (linkRect.top < navRect.top) {
              nav.scrollTop -= navRect.top - linkRect.top;
            } else if (linkRect.bottom > navRect.bottom) {
              nav.scrollTop += linkRect.bottom - navRect.bottom;
            }
            if (linkRect.left < navRect.left) {
              nav.scrollLeft -= navRect.left - linkRect.left;
            } else if (linkRect.right > navRect.right) {
              nav.scrollLeft += linkRect.right - navRect.right;
            }
          }
          updateControls();
          placeFeedback();
          warmNearbyImages();
          if (options.announce) {
            active.scrollIntoView({ block: "start", behavior: "instant" });
            announce(
              `${active.querySelector("h1, h2")?.textContent || "Match"} selected. ${visibleGroups().length} matches shown`,
            );
          }
        };

        const setOverview = (enabled, options = {}) => {
          document.body.classList.toggle("overview-mode", enabled);
          overview?.setAttribute("aria-pressed", enabled ? "true" : "false");
          const label = overview?.querySelector(".control-label");
          if (label) label.textContent = enabled ? "Focus view" : "Overview";
          if (options.announce !== false) {
            announce(enabled ? "Overview opened" : "Focus view opened");
          }
        };

        const move = (offset) => {
          const visible = visibleGroups();
          if (!visible.length) return;
          const index = Math.max(0, visibleIndices.get(active) ?? -1);
          const target =
            visible[Math.min(visible.length - 1, Math.max(0, index + offset))];
          setOverview(false);
          setActive(target, { announce: true });
          writeStoredState();
        };

        const syncDecisionUi = (group) => {
          const decision = decisions.get(group.dataset.reviewKey);
          const reviewed = isComplete(decision);
          const wasReviewed = group.dataset.reviewed === "true";
          if (wasReviewed !== reviewed && isQuickWin(group)) quickWinCount += reviewed ? -1 : 1;
          if (reviewed) reviewedKeys.add(group.dataset.reviewKey);
          else reviewedKeys.delete(group.dataset.reviewKey);
          group.dataset.reviewed = reviewed ? "true" : "false";
          group.dataset.decision = decision?.action || "";
          group.querySelectorAll("[data-decision-action]").forEach((button) => {
            button.setAttribute("aria-pressed", decision?.action === button.dataset.decisionAction ? "true" : "false");
          });
          const notice = group.querySelector("[data-decision-notice]");
          if (notice) notice.textContent = decision?.action === "remove" && !reviewed ? `${decision.remove_pin_ids.length} ${decision.remove_pin_ids.length === 1 ? "copy" : "copies"} selected` : "";
          group.querySelectorAll("[data-remove-pin]").forEach((input) => {
            input.checked = Boolean(decision?.remove_pin_ids.includes(input.dataset.removePin));
          });
          const button = group.querySelector("[data-review-button]");
          button?.setAttribute("aria-pressed", reviewed ? "true" : "false");
          const label = button?.querySelector("[data-review-label]");
          if (label) label.textContent = reviewed ? "Reviewed" : decision?.action === "remove" ? "Save decision" : "Mark reviewed";
          const link = linkByTarget.get(group.id);
          link?.classList.toggle("is-reviewed", reviewed);
          if (link) {
            const title = link.querySelector(".match-link-title")?.textContent?.trim() || "Match";
            if (reviewed) link.setAttribute("aria-label", `${title}, reviewed`);
            else link.removeAttribute("aria-label");
          }
        };

        let undoDecisions = null;
        let undoTarget = null;
        let undoKeys = [];
        const feedback = document.querySelector("#review-feedback");
        const placeFeedback = () => {
          const container = active?.querySelector(".match-actions") ||
            (groups.length ? empty : document.querySelector(".review-transfer"));
          if (container && feedback.parentElement !== container) container.append(feedback);
        };
        let feedbackTimer = null;
        const pauseFeedback = () => {
          clearTimeout(feedbackTimer);
          feedbackTimer = null;
        };
        const dismissFeedback = () => {
          const hadFocus = feedback.contains(document.activeElement);
          feedback.hidden = true;
          pauseFeedback();
          undoDecisions = null;
          undoKeys = [];
          undoTarget = null;
          if (hadFocus) focusReview();
        };
        const scheduleFeedbackDismissal = () => {
          pauseFeedback();
          if (feedback.hidden || feedback.matches(":hover") || feedback.contains(document.activeElement)) return;
          feedbackTimer = setTimeout(dismissFeedback, 5000);
        };
        feedback.addEventListener("pointerenter", pauseFeedback);
        feedback.addEventListener("pointerleave", scheduleFeedbackDismissal);
        feedback.addEventListener("focusin", pauseFeedback);
        feedback.addEventListener("focusout", scheduleFeedbackDismissal);
        document.querySelector("#dismiss-feedback")?.addEventListener("click", dismissFeedback);
        document.addEventListener("keydown", (event) => {
          if (event.key === "Escape" && !feedback.hidden) {
            event.preventDefault();
            dismissFeedback();
          }
        });
        const offerUndo = (message, target = active, keys = [target?.dataset.reviewKey].filter(Boolean)) => {
          undoDecisions = new Map(decisions);
          undoTarget = target;
          undoKeys = keys;
          feedback.hidden = false;
          placeFeedback();
          document.querySelector("#review-feedback-label").textContent = message;
          scheduleFeedbackDismissal();
        };
        const focusReview = () => {
          active?.scrollIntoView({ block: "start", behavior: "instant" });
          const target = active?.querySelector("[data-review-button]") || document.querySelector("#clear-filters");
          target?.focus({ preventScroll: true });
        };
        const setDecision = (group, action, removePinIds = [], reviewed = Boolean(action && action !== "later" && action !== "remove")) => {
          if (!group) return;
          let preferredActive = null;
          if (reviewed && unreviewedOnly?.checked && group === active) {
            const index = visibleIndices.get(group) ?? -1;
            preferredActive = visible[index + 1] || visible[index - 1] || null;
          }
          const message = action === "remove" && !reviewed ? "Selection updated" : action ? "Decision saved" : "Decision cleared";
          offerUndo(message, group);
          if (action) decisions.set(group.dataset.reviewKey, decisionRecord(group, action, removePinIds, reviewed));
          else decisions.delete(group.dataset.reviewKey);
          dirtyDecisions.add(group.dataset.reviewKey);
          syncDecisionUi(group);
          if (unreviewedOnly?.checked) applyFilter(preferredActive, group);
          else syncQuickWins();
          updateProgress();
          if (reviewed) acknowledgeReview();
          announce(`${message}. ${visible.length} matches shown`);
        };
        const setReviewed = (group, reviewed) => {
          const current = decisions.get(group.dataset.reviewKey);
          if (reviewed && current?.action === "remove") setDecision(group, "remove", current.remove_pin_ids, true);
          else setDecision(group, reviewed ? "reviewed" : "");
        };

        const advanceAfterReview = (group) => {
          const visible = visibleGroups();
          const index = visibleIndices.get(group) ?? -1;
          if (index < 0 || index >= visible.length - 1) return;
          setOverview(false);
          setActive(visible[index + 1], { announce: true });
        };

        const applyFilter = (preferredActive = null, changedGroup = null) => {
          const scope =
            scopeFilters.find((filter) => filter.checked)?.value || "all";
          const kind =
            kindFilters.find((filter) => filter.checked)?.value || "all";
          syncQuickWins();
          (changedGroup ? [changedGroup] : groups).forEach((group) => {
            const scopeMatches = !(
              scope !== "all" && group.dataset.scope !== scope
            );
            const kindMatches = kind === "all" || group.dataset.kind === kind;
            const reviewMatches =
              !unreviewedOnly?.checked || group.dataset.reviewed !== "true";
            const hidden = !(scopeMatches && kindMatches && reviewMatches && (!quickOnly || isQuickWin(group)));
            group.classList.toggle("is-filtered", hidden);
            const link = linkByTarget.get(group.id);
            link?.classList.toggle("is-filtered", hidden);
          });
          visible = groups.filter(
            (group) => !group.classList.contains("is-filtered"),
          );
          visibleIndices = new Map(visible.map((group, index) => [group, index]));
          if (visibleCount) {
            visibleCount.textContent = `${visible.length} shown`;
            const mobileCount = document.querySelector("[data-mobile-count]");
            if (mobileCount) mobileCount.textContent = `${visible.length} matches`;
          }
          const emptyTitle = document.querySelector("#filter-empty-title");
          if (emptyTitle) {
            emptyTitle.textContent = reviewedKeys.size === groups.length && groups.length > 0
              ? "All matches reviewed"
              : "No matches in the current filters.";
          }
          empty?.classList.toggle(
            "is-visible",
            groups.length > 0 && visible.length === 0,
          );
          if (!visible.includes(active)) {
            // Clear the outgoing match here rather than leaving it to
            // setActive: reassigning first would hand setActive the
            // replacement, stranding "is-active" on the filtered-out
            // group so it reappears beside the real one under "All".
            active?.classList.remove("is-active");
            linkByTarget.get(active?.id)?.setAttribute("aria-current", "false");
            active = visible[0] || null;
            if (visible.includes(preferredActive)) active = preferredActive;
          }
          if (active) setActive(active);
          else { updateControls(); placeFeedback(); }
          announce(`${visible.length} matches shown`);
        };

        const restoreState = () => {
          const stored = readStoredState();
          if (stored) {
            try { decisions = parseDecisions(stored); } catch (_) { return; }
          }
          groups.forEach(syncDecisionUi);
          let view = null;
          try { view = JSON.parse(window.localStorage.getItem(viewStorageKey)); } catch (_) { /* Session defaults still work. */ }
          if (!view || !groupByKey.has(view.active)) {
            if (unreviewedOnly) unreviewedOnly.checked = reviewedKeys.size > 0;
            return;
          }
          quickOnly = view.quickOnly === true;
          const storedScope = scopeFilters.find((filter) => filter.value === view.scope);
          const storedKind = kindFilters.find((filter) => filter.value === view.kind);
          if (storedScope) storedScope.checked = true;
          if (storedKind) storedKind.checked = true;
          if (unreviewedOnly) unreviewedOnly.checked = view.unreviewedOnly === true || reviewedKeys.size > 0;
          active = groupByKey.get(view.active);
          setOverview(view.overview === true, { announce: false });
        };

        links.forEach((link) => {
          const warmTarget = () =>
            warmGroupImages(document.getElementById(link.dataset.target));
          link.addEventListener("pointerenter", warmTarget);
          link.addEventListener("focus", warmTarget);
          link.addEventListener("click", () => {
            setOverview(false);
            setActive(document.getElementById(link.dataset.target), {
              announce: true,
            });
            writeStoredState();
          });
        });

        groups.forEach((group) => {
          group
            .querySelector("[data-review-button]")
            ?.addEventListener("click", () => {
              setActive(group);
              const reviewed = group.dataset.reviewed !== "true";
              setReviewed(group, reviewed);
              if (reviewed) advanceAfterReview(group);
              focusReview();
              writeStoredState();
            });
        });

        [...scopeFilters, ...kindFilters].forEach((filter) =>
          filter.addEventListener("change", () => {
            quickOnly = false;
            applyFilter();
            writeStoredState();
          }),
        );
        quickWins?.addEventListener("click", () => {
          quickOnly = true;
          if (unreviewedOnly) unreviewedOnly.checked = true;
          const sameBoard = scopeFilters.find(
            (filter) => filter.value === "same-board",
          );
          const exact = kindFilters.find((filter) => filter.value === "exact");
          if (sameBoard) sameBoard.checked = true;
          if (exact) exact.checked = true;
          applyFilter();
          writeStoredState();
          announce("Quick wins shown");
        });
        unreviewedOnly?.addEventListener("change", () => {
          applyFilter();
          writeStoredState();
        });
        previous?.addEventListener("click", () => move(-1));
        next?.addEventListener("click", () => move(1));
        overview?.addEventListener("click", () => {
          setOverview(!document.body.classList.contains("overview-mode"));
          if (!document.body.classList.contains("overview-mode")) {
            active?.scrollIntoView({ block: "start", behavior: "instant" });
          }
          writeStoredState();
        });
        resetReview?.addEventListener("click", () => {
          offerUndo("Decisions in this report cleared", active, currentGroupKeys);
          groups.forEach((group) => { decisions.delete(group.dataset.reviewKey); dirtyDecisions.add(group.dataset.reviewKey); syncDecisionUi(group); });
          applyFilter(); updateProgress(); writeStoredState();
          focusReview();
          announce("Decisions in this report cleared");
        });

        document.querySelector("#clear-filters")?.addEventListener("click", () => {
          quickOnly = false;
          scopeFilters.forEach((filter) => filter.checked = filter.value === "all");
          kindFilters.forEach((filter) => filter.checked = filter.value === "all");
          if (unreviewedOnly) unreviewedOnly.checked = false;
          applyFilter();
          focusReview();
          writeStoredState();
        });
        document.querySelector("#undo-review")?.addEventListener("click", () => {
          if (!undoDecisions) return;
          const target = undoTarget;
          undoKeys.forEach((key) => {
            if (undoDecisions.has(key)) decisions.set(key, undoDecisions.get(key));
            else decisions.delete(key);
            dirtyDecisions.add(key);
          });
          dismissFeedback();
          groups.forEach(syncDecisionUi);
          applyFilter(target);
          if (target && !target.classList.contains("is-filtered")) setActive(target, { announce: true });
          updateProgress(); writeStoredState(); focusReview(); announce("Decision change undone");
        });

        groups.forEach((group) => {
          group.querySelectorAll("[data-decision-action]").forEach((button) => button.addEventListener("click", () => {
            const action = button.dataset.decisionAction;
            group.querySelectorAll(".decision-more").forEach((menu) => menu.open = false);
            setDecision(group, action);
            if (action) advanceAfterReview(group);
            focusReview();
            writeStoredState();
          }));
          group.querySelectorAll("[data-remove-pin]").forEach((input) => input.addEventListener("change", () => {
            const ids = [...group.querySelectorAll("[data-remove-pin]:checked")].map((choice) => choice.dataset.removePin);
            setDecision(group, ids.length ? "remove" : "", ids);
            writeStoredState();
          }));
        });
        const transferStatus = document.querySelector("#transfer-status");
        document.querySelector("#export-review")?.addEventListener("click", () => {
          try {
            if (mergeStoredDecisions()) { groups.forEach(syncDecisionUi); applyFilter(); updateProgress(); }
          } catch (_) { /* Export remains available when browser storage is unavailable. */ }
          const blob = new Blob([JSON.stringify({ schema_version: 1, decisions: [...decisions.values()] }, null, 2)], { type: "application/json" });
          const url = URL.createObjectURL(blob);
          const link = document.createElement("a");
          link.href = url; link.download = "unpin-decisions.json"; link.click();
          setTimeout(() => URL.revokeObjectURL(url), 1000);
          transferStatus.textContent = `Exported ${decisions.size} decisions. This file includes pin and image links.`;
        });
        const reviewFile = document.querySelector("#review-file");
        document.querySelector("#import-review")?.addEventListener("click", () => reviewFile.click());
        reviewFile?.addEventListener("change", async () => {
          const file = reviewFile.files[0];
          if (!file) return;
          try {
            if (file.size > 50 * 1024 * 1024) throw new Error("Decisions file exceeds the 50 MiB limit.");
            const imported = parseDecisions(JSON.parse(await file.text()));
            offerUndo("Decisions imported", active, [...imported.keys()]);
            imported.forEach((decision, key) => { decisions.set(key, decision); dirtyDecisions.add(key); });
            groups.forEach(syncDecisionUi);
            if (reviewedKeys.size && unreviewedOnly) unreviewedOnly.checked = true;
            applyFilter(); updateProgress(); writeStoredState();
            transferStatus.textContent = `Imported ${imported.size} decisions; ${[...imported.keys()].filter((key) => groupByKey.has(key)).length} match this report.`;
          } catch (error) { transferStatus.textContent = error.message || "Invalid decisions file."; }
          finally { reviewFile.value = ""; }
        });

        document.querySelector("#toggle-filters")?.addEventListener("click", (event) => {
          const button = event.currentTarget;
          const expanded = button.getAttribute("aria-expanded") !== "true";
          button.setAttribute("aria-expanded", String(expanded));
          button.closest(".rail").dataset.filtersOpen = String(expanded);
        });
        document.querySelectorAll('a[href="#scan-warnings"]').forEach((link) => link.addEventListener("click", () => {
          const warnings = document.querySelector("#scan-warnings");
          if (warnings) warnings.open = true;
          const tools = warnings?.closest(".report-tools");
          if (tools) tools.open = true;
        }));
        window.addEventListener("resize", () => { if (active) setActive(active); });
        window.addEventListener("storage", (event) => {
          if (event.key !== storageKey && event.key !== null) return;
          try {
            if (mergeStoredDecisions()) { groups.forEach(syncDecisionUi); applyFilter(); updateProgress(); }
          } catch (_) { /* Ignore invalid external state without losing this page's edits. */ }
        });
        window.addEventListener("beforeprint", () => groups.forEach(warmGroupImages));

        document.addEventListener("keydown", (event) => {
          if (
            event.metaKey ||
            event.ctrlKey ||
            event.altKey ||
            event.target instanceof HTMLInputElement ||
            event.target instanceof HTMLTextAreaElement ||
            event.target instanceof HTMLSelectElement ||
            event.target instanceof HTMLButtonElement ||
            event.target instanceof HTMLAnchorElement ||
            event.target?.isContentEditable
          ) {
            return;
          }
          const key = event.key.toLowerCase();
          if (key === "j") move(1);
          if (key === "k") move(-1);
          if (key === "e" && active) {
            const reviewedGroup = active;
            const reviewed = reviewedGroup.dataset.reviewed !== "true";
            setReviewed(reviewedGroup, reviewed);
            if (reviewed) advanceAfterReview(reviewedGroup);
            writeStoredState();
          }
          if (key === "o") {
            setOverview(!document.body.classList.contains("overview-mode"));
            writeStoredState();
          }
        });

        initializeImages();
        restoreState();
        applyFilter();
        updateProgress();
      })();
