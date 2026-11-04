using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

public enum UnsavedChoice { Cancel, Discard, Save }

/// <summary>Document interaction only. Persistence and dirty state belong to Rust.</summary>
public sealed partial class MainWindowViewModel
{
    public Func<Task<string?>>? PickProject { get; set; }
    public Func<Task<string?>>? PickRelinkSource { get; set; }
    public Func<string?, Task<string?>>? PickSaveProject { get; set; }
    public Func<Task<UnsavedChoice>>? ConfirmUnsaved { get; set; }

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(SaveProjectCommand), nameof(SaveProjectAsCommand), nameof(OpenProjectCommand), nameof(NewProjectCommand), nameof(RelinkMediaCommand), nameof(ImportCommand))]
    public partial bool DocumentBusy { get; set; }

    [ObservableProperty]
    public partial string ProjectCaption { get; set; } = "Starting…";

    [ObservableProperty]
    public partial string ProjectStateText { get; set; } = "Not saved";

    [ObservableProperty]
    public partial string ProjectPath { get; set; } = "Choose Save As to create a .cutcap project.";

    private bool CanUseDocument() => EngineReady && !DocumentBusy && _engine.View?.Project?.Document?.Busy != true;
    private bool CanRelink() => CanUseDocument() && !Library.ImportBusy && Library.SelectedItem is not null;

    [RelayCommand(CanExecute = nameof(CanUseDocument))]
    private async Task SaveProjectAsync() => await SaveDocumentAsync(false);

    [RelayCommand(CanExecute = nameof(CanUseDocument))]
    private async Task SaveProjectAsAsync() => await SaveDocumentAsync(true);

    private async Task<bool> SaveDocumentAsync(bool saveAs)
    {
        var wasBusy = DocumentBusy;
        DocumentBusy = true;
        try
        {
            var before = _engine.View?.Project?.Document?.OperationId;
            var path = _engine.View?.Project?.Document?.Path;
            if (saveAs || path is null)
            {
                if (PickSaveProject is null) { return false; }
                path = await PickSaveProject(path);
                if (path is null) { return false; }
                if (!_engine.Run(session => session.SaveProjectAs(path))) { return false; }
            }
            else if (!_engine.Run(session => session.SaveProject())) { return false; }
            // Acceptance is not success. The accepted request stays pending until
            // its terminal event; command-owner snapshots carry the saved revision.
            var view = await WaitForProjectCommandsAsync();
            return view?.Project?.Document is { State: "saved" } doc && doc.OperationId != before;
        }
        finally { DocumentBusy = wasBusy; }
    }

    [RelayCommand(CanExecute = nameof(CanUseDocument))]
    private async Task OpenProjectAsync()
    {
        if (PickProject is null) { return; }
        DocumentBusy = true;
        try
        {
            var path = await PickProject();
            if (path is null || !await ConfirmReplacementAsync()) { return; }
            if (_engine.Run(session => session.OpenProject(path))) { await WaitForProjectCommandsAsync(); }
        }
        finally { DocumentBusy = false; }
    }

    [RelayCommand(CanExecute = nameof(CanUseDocument))]
    private async Task NewProjectAsync()
    {
        DocumentBusy = true;
        try
        {
            if (await ConfirmReplacementAsync() && _engine.Run(session => session.NewProject()))
            { await WaitForProjectCommandsAsync(); }
        }
        finally { DocumentBusy = false; }
    }

    [RelayCommand(CanExecute = nameof(CanRelink))]
    private async Task RelinkMediaAsync()
    {
        var id = Library.SelectedItem?.Id;
        if (id is null || PickRelinkSource is null) { return; }
        DocumentBusy = true;
        try
        {
            var path = await PickRelinkSource();
            if (path is not null) { _engine.Run(session => session.RelinkMedia(id, path)); }
        }
        finally { DocumentBusy = false; }
    }

    private async Task<bool> ConfirmReplacementAsync()
    {
        // Drain already accepted mutations/import registrations before checking
        // dirty state. A picker never authorizes discarding newly arrived edits.
        await WaitForProjectCommandsAsync(requireNextView: false);
        if (_engine.View?.Project?.Document?.Dirty != true) { return true; }
        if (ConfirmUnsaved is null) { return false; }
        return await ConfirmUnsaved() switch
        {
            UnsavedChoice.Discard => true,
            UnsavedChoice.Save => await SaveDocumentAsync(false)
                && _engine.View?.Project?.Document?.Dirty == false,
            _ => false,
        };
    }

    public async Task<bool> ConfirmCloseAsync()
    {
        if (DocumentBusy) { ShowRejection("Finish the project dialog or file operation before closing."); return false; }
        DocumentBusy = true;
        try { return await ConfirmReplacementAsync(); }
        finally { DocumentBusy = false; }
    }

    private Task<SessionView?> WaitForProjectCommandsAsync(bool requireNextView = true)
    {
        if (_engine.Session is null || _engine.Session.IsDisposed) { return Task.FromResult<SessionView?>(null); }
        if (!requireNextView && _engine.View is { Pending: 0 } current) { return Task.FromResult<SessionView?>(current); }
        var completion = new TaskCompletionSource<SessionView?>();
        void Changed(object? sender, SessionView view)
        {
            if (view.Pending == 0)
            {
                _engine.ViewChanged -= Changed;
                _engine.CommandRejected -= Rejected;
                completion.TrySetResult(view);
            }
        }
        void Rejected(object? sender, string message)
        {
            _engine.ViewChanged -= Changed;
            _engine.CommandRejected -= Rejected;
            completion.TrySetResult(null);
        }
        _engine.ViewChanged += Changed;
        _engine.CommandRejected += Rejected;
        return completion.Task;
    }

    private void ApplyDocument(ProjectView? project)
    {
        var doc = project?.Document;
        ProjectCaption = ProjectName + (doc?.Dirty == true ? " *" : "");
        ProjectPath = (doc?.Error is { } error ? error + "\n" : "")
            + (doc?.Path ?? "Choose Save As to create a .cutcap project.");
        ProjectStateText = doc?.State switch
        {
            "saving" => "Saving…", "opening" => "Opening…", "failed" => "File operation failed",
            _ => doc?.Dirty == true ? "Unsaved changes" : project?.Saved == true ? "Saved" : "Not saved",
        };
        SaveProjectCommand.NotifyCanExecuteChanged();
        SaveProjectAsCommand.NotifyCanExecuteChanged();
        OpenProjectCommand.NotifyCanExecuteChanged();
        NewProjectCommand.NotifyCanExecuteChanged();
        RelinkMediaCommand.NotifyCanExecuteChanged();
    }
}
