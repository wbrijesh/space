//! Actions shared by the app shell and its components (tooltips show their key bindings).

gpui::actions!(
    space,
    [
        NewThread,
        ToggleSidebar,
        GoBack,
        GoForward,
        FocusComposer,
        OpenSettings,
        CloseSettings,
        CyclePermissionMode,
        ScrollToBottom,
        Quit
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
