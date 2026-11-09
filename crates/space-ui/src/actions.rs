//! Actions shared by the app shell and its components (tooltips show their key bindings).

gpui::actions!(
    space,
    [
        NewThread,
        CloseThread,
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
        ToggleTerminal,
        NewTerminal,
        CloseTerminal,
        NextTerminal,
        PreviousTerminal,
        MaximizeTerminal,
        TerminalCopy,
        TerminalPaste,
        TerminalClear
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
