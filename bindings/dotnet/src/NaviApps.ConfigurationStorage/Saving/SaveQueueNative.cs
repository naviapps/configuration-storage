using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace NaviApps.ConfigurationStorage;

[StructLayout(LayoutKind.Sequential)]
internal struct ConfigurationStorageSaveQueueView
{
    internal ulong Revision, CommittedRevision, PendingRevision, WritingRevision, FailedRevision;
    internal uint HasUnsavedChanges;
}

internal sealed partial class PInvokeStorageNative
{
    internal static SaveQueueHandle CreateSaveQueue()
    {
        ValidateAbi(Instance);
        var owner = SaveQueueCreate();
        if (owner.IsInvalid) { owner.Dispose(); throw new InvalidOperationException("Native save queue is unavailable."); }
        return owner;
    }
    [DllImport(Library, EntryPoint = "configuration_storage_save_queue_create", CallingConvention = CallingConvention.Cdecl)]
    private static extern SaveQueueHandle SaveQueueCreate();
    [DllImport(Library, EntryPoint = "configuration_storage_save_queue_enqueue", CallingConvention = CallingConvention.Cdecl)]
    internal static extern ulong EnqueueSaveQueue(SaveQueueHandle owner);
    [DllImport(Library, EntryPoint = "configuration_storage_save_queue_begin", CallingConvention = CallingConvention.Cdecl)]
    internal static extern ulong BeginSaveQueue(SaveQueueHandle owner);
    [DllImport(Library, EntryPoint = "configuration_storage_save_queue_complete", CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint CompleteSaveQueue(SaveQueueHandle owner, ulong revision, uint succeeded);
    [DllImport(Library, EntryPoint = "configuration_storage_save_queue_view", CallingConvention = CallingConvention.Cdecl)]
    internal static extern ConfigurationStorageSaveQueueView ViewSaveQueue(SaveQueueHandle owner);
    [DllImport(Library, EntryPoint = "configuration_storage_save_queue_destroy", CallingConvention = CallingConvention.Cdecl)]
    internal static extern void DestroySaveQueue(nint owner);
}

internal sealed class SaveQueueHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    public SaveQueueHandle() : base(true) { }
    protected override bool ReleaseHandle() { PInvokeStorageNative.DestroySaveQueue(handle); return true; }
}
