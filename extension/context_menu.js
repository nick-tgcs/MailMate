// context_menu.js — MailMate's message context-menu actions.
//
// Registers menu items on the message list / message display. Each click reads the target
// message and dispatches the matching native request: classify the selected message, draft a
// reply, or teach a correction (mark spam / not spam). The host applies safe outcomes and
// returns suggestions; the menu never sends mail.

/* exported registerContextMenus, MAILMATE_MENU */

const MAILMATE_MENU = {
  classify: "mailmate-classify",
  draft: "mailmate-draft-reply",
  markSpam: "mailmate-mark-spam",
  markNotSpam: "mailmate-mark-not-spam",
};

// Register the menu items (idempotent across event-page restarts via removeAll first).
function registerContextMenus(handlers) {
  browser.menus.removeAll();
  const contexts = ["message_list", "message_display_action_menu"];
  browser.menus.create({ id: MAILMATE_MENU.classify, title: "MailMate: Classify message", contexts });
  browser.menus.create({ id: MAILMATE_MENU.draft, title: "MailMate: Draft reply", contexts });
  browser.menus.create({ id: MAILMATE_MENU.markSpam, title: "MailMate: This is spam", contexts });
  browser.menus.create({
    id: MAILMATE_MENU.markNotSpam,
    title: "MailMate: Not spam",
    contexts,
  });

  browser.menus.onClicked.addListener(async (info) => {
    const message = firstSelected(info);
    if (!message) {
      return;
    }
    switch (info.menuItemId) {
      case MAILMATE_MENU.classify:
        await handlers.classify(message);
        break;
      case MAILMATE_MENU.draft:
        await handlers.draftReply(message);
        break;
      case MAILMATE_MENU.markSpam:
        await handlers.recordCorrection(message, true);
        break;
      case MAILMATE_MENU.markNotSpam:
        await handlers.recordCorrection(message, false);
        break;
      default:
        break;
    }
  });
}

// The first message the menu click targeted, or null.
function firstSelected(info) {
  if (info.selectedMessages && info.selectedMessages.messages.length) {
    return info.selectedMessages.messages[0];
  }
  if (info.displayedMessages && info.displayedMessages.length) {
    return info.displayedMessages[0];
  }
  return null;
}
