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
        const galleryToggle = document.querySelector("#gallery-toggle");
        const focusPosition = document.querySelector("#focus-position");
        const easyCalls = document.querySelector("#easy-calls");
        const empty = document.querySelector("#filter-empty");
        const progress = document.querySelector("#review-progress");
        const progressFill = document.querySelector("#progress-fill");
        const progressBlock = document.querySelector(".progress-block");
        const resetReview = document.querySelector("#reset-review");
        const status = document.querySelector("#interaction-status");
        const storageKey = "unpin:review:v3";
        const removedStorageKey = "unpin:removed:v1";
        // Set when the page is served by `unpin` itself: decisions then live
        // in a file the server owns, and pins can be checked with Pinterest.
        const server = (() => {
          const element = document.querySelector("#unpin-server");
          if (!element) return null;
          try { return JSON.parse(element.textContent); } catch (_) { return null; }
        })();
        const viewStorageKey = "unpin:view:v1";
        let decisions = new Map();
        const dirtyDecisions = new Set();
        let storedRevision = null;
        const actions = new Set(["reviewed", "keep_all", "remove", "later", "not_duplicate"]);
        const groupByKey = new Map(groups.map((group) => [group.dataset.reviewKey, group]));
        // Easy calls: copies on one board that are identical or nearly so.
        // Covers of videos and stories always need a look, and so do copies
        // on different boards, which are often saved there on purpose.
        const EASY_SIMILARITY = 98;
        const isQuickWin = (group) => group.dataset.scope === "same-board" &&
          group.dataset.previewOnly !== "true" &&
          (group.dataset.kind === "exact" || Number(group.dataset.similarity) >= EASY_SIMILARITY);
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

        const announce = (message) => {
          if (status) status.textContent = message;
        };

        const readStoredState = () => {
          if (server) return server.state || null;
          try {
            const value = window.localStorage.getItem(storageKey);
            storedRevision = value;
            return value ? JSON.parse(value) : null;
          } catch (_) {
            return null;
          }
        };

        const mergeStoredDecisions = () => {
          if (server) return false;
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
        let saveChain = Promise.resolve();
        const saveToServer = () => {
          const keys = [...dirtyDecisions];
          dirtyDecisions.clear();
          const body = JSON.stringify({
            set: keys.filter((key) => decisions.has(key)).map((key) => decisions.get(key)),
            remove: keys.filter((key) => !decisions.has(key)),
          });
          saveChain = saveChain.then(() => fetch(`${server.api}/decisions`, {
            method: "PUT", headers: { "Content-Type": "application/json" }, body,
          })).then((response) => {
            if (!response.ok) throw new Error(`HTTP ${response.status}`);
            document.querySelector("#storage-notice").hidden = true;
          }).catch(() => {
            // Keep the edits so the next change sends them again.
            keys.forEach((key) => dirtyDecisions.add(key));
            const notice = document.querySelector("#storage-notice");
            notice.textContent = "Couldn't save to unpin. Check that it is still running in your terminal; your next change will try again.";
            notice.hidden = false;
          });
        };
        const writeStoredState = () => {
          if (server && dirtyDecisions.size) {
            saveToServer();
            return;
          }
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
                gallery: isGallery(),
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

        // Easy calls: identical copies on one board that nobody has decided
        // yet. Accepting keeps the suggested copy and flags the rest.
        const undecidedEasyCalls = () => groups.filter((group) => isQuickWin(group) && !decisions.has(group.dataset.reviewKey));
        const syncEasyCalls = () => {
          if (!easyCalls) return;
          const count = undecidedEasyCalls().length;
          easyCalls.hidden = count === 0;
          const countLabel = document.querySelector("#easy-count");
          if (countLabel) countLabel.textContent = count;
          const noun = document.querySelector("#easy-noun");
          if (noun) noun.textContent = count === 1 ? "duplicate is an easy call" : "duplicates are easy calls";
          const accept = document.querySelector("#accept-easy");
          if (accept) accept.textContent = count === 1 ? "Accept it" : `Accept all ${count}`;
        };
        // The copy to keep: the suggested one, else the largest image. Every
        // other save in the match, including extra saves of the kept image,
        // is a copy to remove.
        const keptPinFor = (group) => {
          const cards = [...group.querySelectorAll(".pin-card[data-pin-id]")];
          const area = (card) => (Number(card.dataset.width) || 0) * (Number(card.dataset.height) || 0);
          const keep = cards.find((card) => card.dataset.recommendation === "keep") ||
            cards.reduce((best, card) => (area(card) > area(best) ? card : best), cards[0]);
          return keep?.dataset.pinId;
        };
        const smallerCopies = (group) => {
          const keep = keptPinFor(group);
          return [...group.querySelectorAll("[data-remove-pin]")]
            .map((input) => input.dataset.removePin)
            .filter((id) => id !== keep);
        };
        const removeSmaller = (group) => {
          const ids = smallerCopies(group);
          if (ids.length) setDecision(group, "remove", ids, true);
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
          syncEasyCalls();
          refreshCleanup();
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
          if (focusPosition) {
            focusPosition.textContent = index < 0 ? ""
              : `${active.querySelector(".match-index")?.textContent || "Match"} · ${index + 1} of ${visible.length} shown`;
          }
        };

        const syncQuickWins = () => {
          if (!quickWins) return;
          const scope = scopeFilters.find((filter) => filter.checked)?.value;
          const kind = kindFilters.find((filter) => filter.checked)?.value;
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
          warmNearbyImages();
          if (options.announce) {
            active.scrollIntoView({ block: "start", behavior: "instant" });
            announce(
              `${active.querySelector("h1, h2")?.textContent || "Match"} selected. ${visibleGroups().length} matches shown`,
            );
          }
        };

        // The gallery shows every match as a card; leaving it focuses one
        // match at a time.
        function isGallery() { return document.body.classList.contains("gallery-mode"); }
        const setGallery = (enabled, options = {}) => {
          const was = isGallery();
          document.body.classList.toggle("gallery-mode", enabled);
          if (options.announce !== false && was !== enabled) {
            announce(enabled ? "Gallery opened" : "Match opened");
          }
          if (enabled && !was && options.restoreFocus !== false) {
            const link = linkByTarget.get(active?.id);
            link?.scrollIntoView({ block: "nearest", behavior: "instant" });
            link?.focus({ preventScroll: true });
          }
        };

        const move = (offset) => {
          const visible = visibleGroups();
          if (!visible.length) return;
          const index = Math.max(0, visibleIndices.get(active) ?? -1);
          const target =
            visible[Math.min(visible.length - 1, Math.max(0, index + offset))];
          setGallery(false);
          setActive(target, { announce: true });
          writeStoredState();
        };

        const syncDecisionUi = (group) => {
          const decision = decisions.get(group.dataset.reviewKey);
          const reviewed = isComplete(decision);
          const wasReviewed = group.dataset.reviewed === "true";
          if (reviewed) reviewedKeys.add(group.dataset.reviewKey);
          else reviewedKeys.delete(group.dataset.reviewKey);
          group.dataset.reviewed = reviewed ? "true" : "false";
          group.dataset.decision = decision?.action || "";
          group.querySelectorAll("[data-decision-action]").forEach((button) => {
            button.setAttribute("aria-pressed", decision?.action === button.dataset.decisionAction ? "true" : "false");
          });
          const notice = group.querySelector("[data-decision-notice]");
          if (notice) notice.textContent = decision?.action === "remove"
            ? `${decision.remove_pin_ids.length} ${decision.remove_pin_ids.length === 1 ? "copy" : "copies"} going to the removal tray`
            : "";
          group.querySelectorAll("[data-remove-pin]").forEach((input) => {
            input.checked = Boolean(decision?.remove_pin_ids.includes(input.dataset.removePin));
          });
          group.querySelector("[data-remove-smaller]")?.setAttribute("aria-pressed", decision?.action === "remove" && reviewed ? "true" : "false");
          group.dataset.draft = decision?.action === "remove" && !reviewed ? "true" : "false";
          document.querySelectorAll(`[data-quick][data-target="${CSS.escape(group.id)}"]`).forEach((quick) => {
            quick.setAttribute("aria-pressed", quick.dataset.quick === "remove"
              ? (decision?.action === "remove" && reviewed ? "true" : "false")
              : (decision?.action === quick.dataset.quick ? "true" : "false"));
          });
          const link = linkByTarget.get(group.id);
          link?.classList.toggle("is-reviewed", reviewed);
          if (link) link.dataset.decision = decision?.action || "";
          const chip = link?.querySelector("[data-chip]");
          if (chip) {
            const flagged = decision?.remove_pin_ids?.length || 0;
            chip.textContent = !decision ? ""
              : decision.action === "remove" ? `${flagged} in tray${reviewed ? "" : " · draft"}`
              : decision.action === "keep_all" ? (smallerCopies(group).length === 1 ? "Kept both" : "Kept all")
              : decision.action === "not_duplicate" ? "Not duplicates"
              : decision.action === "later" ? "Later"
              : "Reviewed";
          }
        };

        let undoDecisions = null;
        let undoTarget = null;
        let undoKeys = [];
        const feedback = document.querySelector("#review-feedback");
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
          document.querySelector("#review-feedback-label").textContent = message;
          scheduleFeedbackDismissal();
        };
        // Opening a match from the gallery moves focus into the page, so the
        // J, K, R and A shortcuts work straight away and Tab reaches the match.
        const focusMatch = () => document.querySelector("#report-content")?.focus({ preventScroll: true });
        const focusReview = () => {
          active?.scrollIntoView({ block: "start", behavior: "instant" });
          const target = active?.querySelector("[data-remove-smaller]") || document.querySelector("#clear-filters");
          target?.focus({ preventScroll: true });
        };
        const setDecision = (group, action, removePinIds = [], reviewed = Boolean(action && action !== "later" && action !== "remove")) => {
          if (!group) return;
          let preferredActive = null;
          if (reviewed && unreviewedOnly?.checked && group === active) {
            const index = visibleIndices.get(group) ?? -1;
            preferredActive = visible[index + 1] || visible[index - 1] || null;
          }
          const message = !action ? "Decision cleared"
            : action === "remove" && !reviewed ? "Selection saved as draft"
            : action === "later" ? "Decision deferred" : "Decision saved";
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

        const advanceAfterReview = (group) => {
          const visible = visibleGroups();
          const index = visibleIndices.get(group) ?? -1;
          if (index < 0 || index >= visible.length - 1) return;
          setGallery(false);
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
          if (visibleCount) visibleCount.textContent = `${visible.length} shown`;
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
          else updateControls();
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
          setGallery(view.gallery !== false, { announce: false, restoreFocus: false });
        };

        links.forEach((link) => {
          const warmTarget = () =>
            warmGroupImages(document.getElementById(link.dataset.target));
          link.addEventListener("pointerenter", warmTarget);
          link.addEventListener("focus", warmTarget);
          link.addEventListener("click", () => {
            setCleanup(false);
            setGallery(false);
            setActive(document.getElementById(link.dataset.target), {
              announce: true,
            });
            focusMatch();
            writeStoredState();
          });
        });

        groups.forEach((group) => {
          const extra = smallerCopies(group).length;
          const exact = group.dataset.kind === "exact";
          const removeLabel = group.querySelector("[data-remove-smaller-label]");
          if (removeLabel) removeLabel.textContent = extra === 1
            ? (exact ? "Remove the duplicate" : "Remove the smaller copy")
            : `Remove ${extra} ${exact ? "duplicates" : "smaller copies"}`;
          const keepLabel = group.querySelector("[data-keep-label]");
          if (keepLabel && extra === 1) keepLabel.textContent = "Keep both";
        });

        // Mark the copies that Remove the smaller copy would take, and the
        // one it keeps, until a decision is made.
        groups.forEach((group) => {
          const keep = keptPinFor(group);
          group.querySelectorAll(".pin-card[data-pin-id]").forEach((card) => {
            const kept = card.dataset.pinId === keep;
            card.dataset.suggested = kept ? "keep" : "remove";
            const tag = card.querySelector(".suggestion");
            if (tag) tag.textContent = kept ? "Keep" : "Suggested to remove";
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
          const allKinds = kindFilters.find((filter) => filter.value === "all");
          if (sameBoard) sameBoard.checked = true;
          if (allKinds) allKinds.checked = true;
          applyFilter();
          if (visible.length) {
            setGallery(false);
            setActive(visible[0], { announce: true });
          }
          writeStoredState();
          announce("Easy calls shown");
        });
        document.querySelector("#accept-easy")?.addEventListener("click", () => {
          const accepted = undecidedEasyCalls();
          if (!accepted.length) return;
          offerUndo(`${accepted.length} easy ${accepted.length === 1 ? "call" : "calls"} accepted`, active, accepted.map((group) => group.dataset.reviewKey));
          let flagged = 0;
          accepted.forEach((group) => {
            const ids = smallerCopies(group);
            if (!ids.length) return;
            flagged += ids.length;
            decisions.set(group.dataset.reviewKey, decisionRecord(group, "remove", ids, true));
            dirtyDecisions.add(group.dataset.reviewKey);
            syncDecisionUi(group);
          });
          quickOnly = false;
          applyFilter();
          updateProgress();
          acknowledgeReview();
          writeStoredState();
          announce(`${flagged} ${flagged === 1 ? "copy" : "copies"} added to the removal tray`);
          document.querySelector("#tray-start")?.focus({ preventScroll: true });
        });
        unreviewedOnly?.addEventListener("change", () => {
          applyFilter();
          writeStoredState();
        });
        previous?.addEventListener("click", () => move(-1));
        next?.addEventListener("click", () => move(1));
        galleryToggle?.addEventListener("click", () => {
          setGallery(true);
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
            setDecision(group, action);
            if (action) advanceAfterReview(group);
            focusReview();
            writeStoredState();
          }));
          group.querySelector("[data-remove-smaller]")?.addEventListener("click", () => {
            setActive(group);
            removeSmaller(group);
            advanceAfterReview(group);
            focusReview();
            writeStoredState();
          });
          // Picking a copy saves straight away. With two saves the pick is
          // the whole decision, so the next match opens.
          group.querySelectorAll("[data-remove-pin]").forEach((input) => input.addEventListener("change", () => {
            const choices = [...group.querySelectorAll("[data-remove-pin]")];
            const ids = choices.filter((choice) => choice.checked).map((choice) => choice.dataset.removePin);
            setActive(group);
            setDecision(group, ids.length ? "remove" : "", ids, ids.length > 0);
            if (input.checked && choices.length === 2 && ids.length === 1) {
              advanceAfterReview(group);
              if (active !== group) focusReview();
            }
            writeStoredState();
          }));
        });
        // One-tap decisions on the gallery cards, without opening the match.
        document.querySelectorAll("[data-quick]").forEach((quick) => quick.addEventListener("click", () => {
          const group = document.getElementById(quick.dataset.target);
          if (!group) return;
          setActive(group);
          if (quick.dataset.quick === "remove") removeSmaller(group);
          else setDecision(group, quick.dataset.quick);
          writeStoredState();
        }));
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
          if (event.key === "Escape" && !event.defaultPrevented && !isGallery() &&
              !document.body.classList.contains("cleanup-mode") && active &&
              !(event.target instanceof HTMLInputElement) && !event.target?.closest?.("details[open]")) {
            setGallery(true);
            writeStoredState();
            return;
          }
          if (
            event.metaKey ||
            event.ctrlKey ||
            event.altKey ||
            event.target instanceof HTMLInputElement ||
            event.target instanceof HTMLTextAreaElement ||
            event.target instanceof HTMLSelectElement ||
            event.target?.isContentEditable
          ) {
            return;
          }
          const key = event.key.toLowerCase();
          if (document.body.classList.contains("cleanup-mode")) {
            if (key === "escape") setCleanup(false);
            return;
          }
          if (key === "j") move(1);
          if (key === "k") move(-1);
          if ((key === "r" || key === "a") && active && !isGallery()) {
            const group = active;
            if (key === "r") removeSmaller(group);
            else setDecision(group, "keep_all");
            advanceAfterReview(group);
            writeStoredState();
          }
          if (key === "o") {
            if (isGallery()) {
              if (active) { setGallery(false); setActive(active, { announce: true }); focusMatch(); }
            } else {
              setGallery(true);
            }
            writeStoredState();
          }
        });


        // Cleanup: the pins flagged for removal, removed one at a time on
        // Pinterest. Pins open in one reused Pinterest tab; on return, the
        // review server checks whether the pin is gone and moves on.
        const cleanup = document.querySelector("#cleanup");
        const cleanupList = document.querySelector("#cleanup-list");
        const cleanupToggle = document.querySelector("#cleanup-toggle");
        const cleanupCount = document.querySelector("#cleanup-count");
        const cleanupProgress = document.querySelector("#cleanup-progress");
        const cleanupEmpty = document.querySelector("#cleanup-empty");
        const checkAll = document.querySelector("#cleanup-check-all");
        const removedPins = (() => {
          if (server) return new Map(Object.entries(server.state?.removed_pins || {}));
          try {
            return new Map(Object.entries(JSON.parse(window.localStorage.getItem(removedStorageKey)) || {}));
          } catch (_) { return new Map(); }
        })();
        const rowStatus = new Map();
        let pendingPin = null;
        let pendingSince = 0;

        const saveRemovedLocally = () => {
          try {
            window.localStorage.setItem(removedStorageKey, JSON.stringify(Object.fromEntries(removedPins)));
          } catch (_) { /* Removal marks then last for this session only. */ }
        };
        const syncRemovedCards = () => {
          document.querySelectorAll("[data-group] [data-pin-id]").forEach((element) => {
            element.dataset.removed = removedPins.has(element.dataset.pinId) ? "true" : "false";
          });
        };
        const setRemoved = (pinId, removed, options = {}) => {
          if (removed) removedPins.set(pinId, Math.floor(Date.now() / 1000));
          else removedPins.delete(pinId);
          if (server && options.persist !== false) {
            fetch(`${server.api}/pins/${encodeURIComponent(pinId)}/removed`, {
              method: "POST", headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ removed }),
            }).catch(() => { document.querySelector("#storage-notice").hidden = false; });
          } else if (!server) {
            saveRemovedLocally();
          }
          syncRemovedCards();
        };

        // One entry per flagged pin in this report, grouped by board.
        const flaggedPins = () => {
          const pins = new Map();
          groups.forEach((group) => {
            const decision = decisions.get(group.dataset.reviewKey);
            if (decision?.action !== "remove") return;
            const label = group.querySelector(".match-index")?.textContent || "Match";
            const everyCopy = decision.remove_pin_ids.length >= decision.items.length;
            decision.remove_pin_ids.forEach((pinId) => {
              const element = group.querySelector(`[data-pin-id="${CSS.escape(pinId)}"]`);
              const record = decision.items.find((item) => item.pin_id === pinId);
              if (!element || !record) return;
              const existing = pins.get(pinId);
              if (existing) {
                existing.matches.push({ label, group });
                existing.everyCopy ||= everyCopy;
                return;
              }
              pins.set(pinId, {
                pinId, pinUrl: record.pin_url, imageUrl: record.image_url,
                board: element.dataset.board || "",
                size: element.querySelector(".dimensions")?.textContent || "",
                matches: [{ label, group }], everyCopy,
              });
            });
          });
          return [...pins.values()];
        };

        const statusText = (pinId) => {
          if (removedPins.has(pinId)) return "Removed";
          const status = rowStatus.get(pinId);
          if (status?.state === "checking") return "Checking with Pinterest…";
          if (status?.state === "present") return status.board ? `Still on Pinterest, on ${status.board}` : "Still on Pinterest";
          if (status?.state === "unknown") return `Couldn't check${status.message ? `: ${status.message}` : ""}. If you deleted it, mark it removed.`;
          if (status?.state === "returned") return "Back from Pinterest. If you deleted it, mark it removed.";
          return "To remove";
        };
        const rowState = (pinId) => removedPins.has(pinId) ? "removed" : rowStatus.get(pinId)?.state || "pending";

        const element = (tag, className, text) => {
          const node = document.createElement(tag);
          if (className) node.className = className;
          if (text !== undefined) node.textContent = text;
          return node;
        };

        const buildRow = (pin) => {
          const row = element("li", "cleanup-row");
          row.dataset.pinId = pin.pinId;
          row.dataset.state = rowState(pin.pinId);
          const thumb = element("img", "cleanup-thumb");
          thumb.src = pin.imageUrl; thumb.alt = ""; thumb.loading = "lazy";
          thumb.referrerPolicy = "no-referrer";
          const copy = element("div", "cleanup-copy");
          copy.append(element("span", "cleanup-title", `Pin ${pin.pinId}`));
          const meta = element("span", "cleanup-meta");
          if (pin.size) meta.append(element("span", "", pin.size));
          pin.matches.forEach(({ label, group }) => {
            const jump = element("button", "cleanup-jump", label);
            jump.type = "button";
            jump.setAttribute("aria-label", `Show ${label}`);
            jump.addEventListener("click", () => {
              setCleanup(false);
              if (group.classList.contains("is-filtered")) document.querySelector("#clear-filters")?.click();
              setGallery(false, { announce: false });
              setActive(group, { announce: true });
              writeStoredState();
            });
            meta.append(jump);
          });
          copy.append(meta);
          if (pin.everyCopy) copy.append(element("span", "cleanup-warning", "Every copy in this match is flagged. Keep one if you still want the image."));
          const status = element("span", "cleanup-status", statusText(pin.pinId));
          status.dataset.cleanupStatus = "";
          copy.append(status);
          const actions = element("div", "cleanup-row-actions");
          const open = element("a", "button primary", "Open on Pinterest");
          open.href = pin.pinUrl; open.target = "unpin-pinterest"; open.rel = "noreferrer";
          open.dataset.cleanupOpen = "";
          open.setAttribute("aria-label", `Open pin ${pin.pinId} on Pinterest`);
          open.addEventListener("click", (event) => {
            event.preventDefault();
            openOnPinterest(pin.pinUrl);
            pendingPin = pin.pinId;
            pendingSince = Date.now();
            rowStatus.delete(pin.pinId);
            currentPinId = pin.pinId;
            renderCurrent(flaggedPins());
            updateRow(pin.pinId);
          });
          actions.append(open);
          if (server?.checks) {
            const check = element("button", "control", "Check");
            check.type = "button";
            check.dataset.cleanupCheck = "";
            check.setAttribute("aria-label", `Check pin ${pin.pinId} with Pinterest`);
            check.addEventListener("click", () => checkPin(pin.pinId, { advance: false }));
            actions.append(check);
          }
          const mark = element("button", "control", "");
          mark.type = "button";
          mark.dataset.cleanupMark = "";
          mark.addEventListener("click", () => {
            const removed = !removedPins.has(pin.pinId);
            setRemoved(pin.pinId, removed);
            rowStatus.delete(pin.pinId);
            updateRow(pin.pinId);
            updateCleanupProgress();
            if (removed) advanceCleanup(pin.pinId);
            announce(removed ? `Pin ${pin.pinId} marked removed` : `Pin ${pin.pinId} back on the list`);
          });
          actions.append(mark);
          row.append(thumb, copy, actions);
          return row;
        };

        const updateRow = (pinId) => {
          const row = cleanupList?.querySelector(`.cleanup-row[data-pin-id="${CSS.escape(pinId)}"]`);
          if (!row) return;
          row.dataset.state = rowState(pinId);
          row.querySelector("[data-cleanup-status]").textContent = statusText(pinId);
          const mark = row.querySelector("[data-cleanup-mark]");
          const removed = removedPins.has(pinId);
          mark.textContent = removed ? "Undo" : "Mark removed";
          mark.setAttribute("aria-label", removed ? `Put pin ${pinId} back on the list` : `Mark pin ${pinId} removed`);
          if (pinId === currentPinId) updateCurrentStatus();
        };

        const updateCleanupProgress = () => {
          const pins = flaggedPins();
          const removed = pins.filter((pin) => removedPins.has(pin.pinId)).length;
          const remaining = pins.length - removed;
          if (cleanupCount) { cleanupCount.textContent = remaining; cleanupCount.hidden = remaining === 0; }
          cleanupToggle?.setAttribute("aria-label", `Remove on Pinterest, ${remaining} ${remaining === 1 ? "pin" : "pins"} left`);
          if (cleanupProgress) cleanupProgress.textContent = `${removed} of ${pins.length}`;
          if (cleanupEmpty) cleanupEmpty.hidden = pins.length > 0;
          if (checkAll) checkAll.hidden = !server?.checks || remaining === 0;
          const done = document.querySelector("#cleanup-done");
          if (done) done.hidden = !(pins.length > 0 && remaining === 0);
          renderTray(pins.filter((pin) => !removedPins.has(pin.pinId)));
          if (document.body.classList.contains("cleanup-mode")) renderCurrent(pins);
        };

        const tray = document.querySelector("#tray");
        const renderTray = (waiting) => {
          if (!tray) return;
          tray.hidden = waiting.length === 0;
          const thumbs = waiting.slice(0, 5).map((pin) => {
            const image = element("img", "tray-thumb");
            image.src = pin.imageUrl; image.alt = ""; image.referrerPolicy = "no-referrer";
            return image;
          });
          document.querySelector("#tray-thumbs")?.replaceChildren(...thumbs);
          const count = document.querySelector("#tray-count");
          if (count) count.textContent = `${waiting.length} ${waiting.length === 1 ? "pin" : "pins"}`;
        };

        // The pin to remove next, large, with what stays behind.
        let currentPinId = null;
        const currentCard = document.querySelector("#cleanup-current");
        const keepFlagged = (pin) => {
          pin.matches.forEach(({ group }) => {
            const decision = decisions.get(group.dataset.reviewKey);
            if (!decision || decision.action !== "remove") return;
            const ids = decision.remove_pin_ids.filter((id) => id !== pin.pinId);
            if (ids.length) setDecision(group, "remove", ids, decision.reviewed);
            else setDecision(group, "keep_all");
          });
          writeStoredState();
          announce(`Pin ${pin.pinId} stays on Pinterest and is out of the tray`);
        };
        const renderCurrent = (pins) => {
          if (!currentCard) return;
          const waiting = pins.filter((pin) => !removedPins.has(pin.pinId));
          const pin = waiting.find((candidate) => candidate.pinId === currentPinId) || waiting[0];
          currentCard.hidden = !pin;
          if (!pin) { currentPinId = null; currentCard.replaceChildren(); return; }
          currentPinId = pin.pinId;
          const { group } = pin.matches[0];
          const decision = decisions.get(group.dataset.reviewKey);
          const keptId = decision?.items.map((item) => item.pin_id).find((id) => !decision.remove_pin_ids.includes(id));
          const keptSize = keptId ? group.querySelector(`[data-pin-id="${CSS.escape(keptId)}"] .dimensions`)?.textContent : null;

          const image = element("img", "current-image");
          image.src = pin.imageUrl; image.alt = ""; image.referrerPolicy = "no-referrer";
          const copy = element("div", "current-copy");
          copy.append(element("p", "current-kicker", `Next up · ${waiting.indexOf(pin) + 1} of ${waiting.length} left`));
          copy.append(element("h3", "current-title", `Pin ${pin.pinId}${pin.board ? ` · ${pin.board}` : ""}`));
          const what = pin.size ? `The ${pin.size} copy.` : "This copy.";
          copy.append(element("p", "current-detail", keptId
            ? `${what} ${keptSize ? `Your ${keptSize} copy stays.` : `Pin ${keptId} stays.`}`
            : `${what} Every copy in this match is flagged, so none will stay.`));
          const actions = element("div", "current-actions");
          const open = element("a", "primary-action", "Open on Pinterest");
          open.href = pin.pinUrl; open.target = "unpin-pinterest"; open.rel = "noreferrer";
          open.dataset.currentOpen = "";
          open.addEventListener("click", (event) => {
            event.preventDefault();
            openOnPinterest(pin.pinUrl);
            pendingPin = pin.pinId;
            pendingSince = Date.now();
            rowStatus.delete(pin.pinId);
            updateRow(pin.pinId);
            updateCurrentStatus();
          });
          const keep = element("button", "text-action", "Keep this one");
          keep.type = "button";
          keep.addEventListener("click", () => keepFlagged(pin));
          const mark = element("button", "text-action", server?.checks ? "Check now" : "I deleted it");
          mark.type = "button";
          mark.dataset.currentMark = "";
          mark.addEventListener("click", () => {
            if (server?.checks) { checkPin(pin.pinId, { advance: true }); return; }
            setRemoved(pin.pinId, true);
            rowStatus.delete(pin.pinId);
            updateRow(pin.pinId);
            updateCleanupProgress();
            advanceCleanup(pin.pinId);
          });
          actions.append(open, mark, keep);
          const status = element("p", "current-status", "");
          status.setAttribute("role", "status");
          status.dataset.currentStatus = "";
          copy.append(actions, status);
          currentCard.replaceChildren(image, copy);
          updateCurrentStatus();
        };
        const updateCurrentStatus = () => {
          const status = currentCard?.querySelector("[data-current-status]");
          if (!status || !currentPinId) return;
          const state = rowStatus.get(currentPinId)?.state;
          status.textContent = state ? statusText(currentPinId)
            : pendingPin === currentPinId
              ? (server?.checks ? "Delete it on Pinterest, then come back here. unpin checks it's gone." : "Delete it on Pinterest, then come back and choose I deleted it.")
              : "";
        };

        const renderCleanupList = () => {
          if (!cleanupList) return;
          const byBoard = new Map();
          flaggedPins().forEach((pin) => {
            const board = pin.board || "Pins";
            if (!byBoard.has(board)) byBoard.set(board, []);
            byBoard.get(board).push(pin);
          });
          const sections = [...byBoard].map(([board, pins]) => {
            const section = element("section", "cleanup-board");
            const heading = element("h3", "", board);
            heading.append(element("span", "cleanup-board-count", ` ${pins.length} ${pins.length === 1 ? "pin" : "pins"}`));
            const list = element("ol", "cleanup-rows");
            pins.forEach((pin) => list.append(buildRow(pin)));
            section.append(heading, list);
            return section;
          });
          cleanupList.replaceChildren(...sections);
          flaggedPins().forEach((pin) => updateRow(pin.pinId));
        };

        function refreshCleanup() {
          if (!cleanup) return;
          if (document.body.classList.contains("cleanup-mode")) renderCleanupList();
          updateCleanupProgress();
        }

        function setCleanup(enabled) {
          if (!cleanup) return;
          if (enabled === document.body.classList.contains("cleanup-mode")) return;
          document.body.classList.toggle("cleanup-mode", enabled);
          cleanup.hidden = !enabled;
          cleanupToggle?.setAttribute("aria-pressed", enabled ? "true" : "false");
          document.querySelector("#decide-step")?.setAttribute("aria-current", enabled ? "false" : "step");
          cleanupToggle?.setAttribute("aria-current", enabled ? "step" : "false");
          if (enabled) {
            setGallery(true, { announce: false, restoreFocus: false });
            renderCleanupList();
            updateCleanupProgress();
            window.scrollTo({ top: 0, behavior: "instant" });
            const first = currentCard?.querySelector("[data-current-open]");
            (first || document.querySelector("#cleanup-back"))?.focus({ preventScroll: true });
            announce("Removal step opened");
          } else {
            pendingPin = null;
            applyFilter();
            setGallery(true, { announce: false });
            announce("Decide step opened");
          }
        }

        // Reuses one named Pinterest tab. The tab keeps its link to this page,
        // because browsers forget the name of a tab whose opener is cut once
        // it moves to another site, and a fresh tab would open every time.
        // The page's no-referrer policy keeps its address out of Pinterest.
        const openOnPinterest = (url) => {
          let tab = null;
          try { tab = window.open("", "unpin-pinterest"); } catch (_) { tab = null; }
          if (!tab) {
            window.open(url, "_blank", "noopener,noreferrer");
            return;
          }
          try {
            tab.location.href = url;
            tab.focus();
          } catch (_) {
            window.open(url, "_blank", "noopener,noreferrer");
          }
        };

        const advanceCleanup = (fromPinId) => {
          if (currentCard && document.body.classList.contains("cleanup-mode")) {
            const pins = flaggedPins().filter((pin) => !removedPins.has(pin.pinId));
            if (!pins.length) {
              document.querySelector("#cleanup-done")?.focus?.();
              announce("Every pin in the tray is gone from Pinterest");
              return;
            }
            const order = flaggedPins().map((pin) => pin.pinId);
            const from = order.indexOf(fromPinId);
            const nextPin = [...order.slice(from + 1), ...order.slice(0, Math.max(0, from))].find((id) => !removedPins.has(id));
            currentPinId = nextPin || pins[0].pinId;
            renderCurrent(flaggedPins());
            currentCard.querySelector("[data-current-open]")?.focus({ preventScroll: true });
            return;
          }
          const rows = [...(cleanupList?.querySelectorAll(".cleanup-row") || [])];
          const index = rows.findIndex((row) => row.dataset.pinId === fromPinId);
          const next = [...rows.slice(index + 1), ...rows.slice(0, Math.max(0, index))]
            .find((row) => row.dataset.state !== "removed");
          if (!next) {
            document.querySelector("#cleanup-done")?.focus?.();
            announce("Every flagged pin is removed");
            return;
          }
          next.scrollIntoView({ block: "nearest", behavior: "instant" });
          next.querySelector("[data-cleanup-open]")?.focus({ preventScroll: true });
        };

        const checkPin = async (pinId, options = {}) => {
          if (!server?.checks) return;
          rowStatus.set(pinId, { state: "checking" });
          updateRow(pinId);
          let result;
          try {
            const response = await fetch(`${server.api}/pins/${encodeURIComponent(pinId)}`);
            if (!response.ok) throw new Error(`HTTP ${response.status}`);
            result = await response.json();
          } catch (_) {
            result = { state: "unknown", message: "unpin isn't responding. Check that it is still running." };
          }
          if (result.state === "removed") {
            rowStatus.delete(pinId);
            // The server already recorded it.
            setRemoved(pinId, true, { persist: false });
          } else {
            if (result.state === "present" && removedPins.has(pinId)) setRemoved(pinId, false, { persist: false });
            rowStatus.set(pinId, result);
          }
          updateRow(pinId);
          updateCleanupProgress();
          if (options.advance && result.state === "removed") {
            announce(`Pin ${pinId} is gone from Pinterest`);
            advanceCleanup(pinId);
          } else if (options.advance) {
            const target = pinId === currentPinId && currentCard
              ? currentCard.querySelector(result.state === "present" ? "[data-current-open]" : "[data-current-mark]")
              : cleanupList?.querySelector(`.cleanup-row[data-pin-id="${CSS.escape(pinId)}"] ${result.state === "present" ? "[data-cleanup-open]" : "[data-cleanup-mark]"}`);
            target?.focus({ preventScroll: true });
            announce(statusText(pinId));
          }
          return result;
        };

        const returnedFromPinterest = () => {
          if (!pendingPin || document.visibilityState !== "visible" || Date.now() - pendingSince < 800) return;
          const pinId = pendingPin;
          pendingPin = null;
          if (server?.checks) {
            checkPin(pinId, { advance: true });
          } else {
            rowStatus.set(pinId, { state: "returned" });
            updateRow(pinId);
            const target = pinId === currentPinId && currentCard
              ? currentCard.querySelector("[data-current-mark]")
              : cleanupList?.querySelector(`.cleanup-row[data-pin-id="${CSS.escape(pinId)}"] [data-cleanup-mark]`);
            target?.focus({ preventScroll: true });
          }
        };
        document.addEventListener("visibilitychange", returnedFromPinterest);
        window.addEventListener("focus", returnedFromPinterest);

        checkAll?.addEventListener("click", async () => {
          checkAll.disabled = true;
          const queue = flaggedPins().map((pin) => pin.pinId).filter((pinId) => !removedPins.has(pinId));
          const worker = async () => {
            while (queue.length) await checkPin(queue.shift());
          };
          await Promise.all([worker(), worker(), worker(), worker()]);
          checkAll.disabled = false;
          const left = flaggedPins().filter((pin) => !removedPins.has(pin.pinId)).length;
          announce(left ? `${left} flagged ${left === 1 ? "pin is" : "pins are"} still on Pinterest` : "Every flagged pin is removed");
        });
        cleanupToggle?.addEventListener("click", () => setCleanup(true));
        document.querySelector("#decide-step")?.addEventListener("click", () => setCleanup(false));
        document.querySelector("#tray-start")?.addEventListener("click", () => setCleanup(true));
        document.querySelector("#cleanup-back")?.addEventListener("click", () => {
          setCleanup(false);
        });
        const cleanupHelp = document.querySelector("#cleanup-help");
        if (cleanupHelp) {
          cleanupHelp.textContent = server?.checks
            ? "Each pin opens in the same Pinterest tab. Delete it there from the pin's ⋯ menu, then come back. unpin checks with Pinterest and moves on to the next pin."
            : "Each pin opens in the same Pinterest tab. Delete it there from the pin's ⋯ menu, then come back and mark it removed.";
          if (server?.checks && !server.signed_in) {
            const note = element("span", "cleanup-note", " Checks are signed out, so a pin on a secret board can look removed. Scan with ");
            note.append(element("code", "", "--cookies-from-browser"), " for reliable checks.");
            cleanupHelp.append(note);
          }
        }
        if (server) {
          const where = server.store_path ? `Decisions are saved to ${server.store_path}.` : "Decisions are saved by unpin.";
          const notice = document.querySelector(".review-notice");
          if (notice) notice.textContent = `${where} Open a pin to make changes on Pinterest.`;
          const footer = document.querySelector("footer");
          if (footer) footer.textContent = `This review page is served by unpin on your computer and loads images from Pinterest. ${where} unpin never changes your Pinterest account.`;
          const storageNotice = document.querySelector("#storage-notice");
          if (storageNotice) storageNotice.textContent = "Couldn't save to unpin. Check that it is still running in your terminal.";
        }
        syncRemovedCards();

        initializeImages();
        restoreState();
        applyFilter();
        updateProgress();
      })();
