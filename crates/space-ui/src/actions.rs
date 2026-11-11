//! Actions shared by the app shell and its components (tooltips show their key bindings).

gpui::actions!(
    space,
    [
        NewThread,
        CloseThread,
        RenameThread,
        TogglePinThread,
        OpenInLastApp,
        NextThread,
        PreviousThread,
        ToggleSidebar,
        ToggleTrajectory,
        GoBack,
        GoForward,
        FocusComposer,
        OpenSettings,
        CloseSettings,
        CyclePermissionMode,
        ScrollToBottom,
        RecallPrevious,
        RecallNext,
        Quit,
        ToggleRightPane,
        ShowTerminal,
        ShowFiles,
        NewTerminal,
        CloseTerminal,
        NextTerminal,
        PreviousTerminal,
        MaximizeTerminal,
        TerminalCopy,
        TerminalPaste,
        TerminalClear,
        OpenFind,
        FindNext,
        FindPrevious,
        CloseFind
    ]
);

/// Dev tooling (gallery window, animation clock).
pub mod dev {
    gpui::actions!(
        space_dev,
        [
            OpenGallery,
            ToggleAnimations,
            StepAnimations,
            AnimationSpeedTenth,
            AnimationSpeedQuarter,
            AnimationSpeedNormal,
            UnpinAnimations
        ]
    );
}
