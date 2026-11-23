using NaviApps.ConfigurationStorage;

static class TestAssertions
{
    internal static void Check(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }

    internal static async Task RejectAsync(Task task, Type type)
    {
        try { await task; }
        catch (Exception error) when (type.IsInstanceOfType(error)) { return; }
        throw new InvalidOperationException("Expected " + type.Name);
    }

    internal static async Task FailureAsync(Task task, ErrorCode code)
    {
        try { await task; }
        catch (StorageException error) when (error.Failure.Code == code) { return; }
        throw new InvalidOperationException("Expected storage failure " + code);
    }
}
