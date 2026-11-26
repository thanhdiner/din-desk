namespace MuseDesk;

internal static class AccountTests
{
    private sealed class Proxy : IAccountProxy
    {
        public bool Running {get;private set;}
        public Settings? Current {get;private set;}
        public bool FailNew {get;set;}
        public bool FailOld {get;set;}
        public string? OldId {get;set;}
        public Task StartAsync(Settings value) { if(Running)throw new Exception("Two concurrent accounts");Current=value;Running=true;return Task.CompletedTask; }
        public Task<int> CheckMuseAsync()
        {
            if((FailNew&&Current?.ActiveAccountId!=OldId)||(FailOld&&Current?.ActiveAccountId==OldId))throw new InvalidOperationException("fixture rejected");
            return Task.FromResult(0);
        }
        public void Stop(){Running=false;}
    }
    public static async Task RunAsync(string root, List<string> checks)
    {
        void Require(bool value,string error) { if(!value)throw new InvalidOperationException(error); }
        var store = new SettingsStore(root);
        string oldKey = store.Load().ApiKey;
        var old = new SettingsStore(root).Load();
        Require(old.ApiKey == oldKey, "Proxy API key changed before the first account was added");
        old.Cookie = "hatch_sess=account-one-secret";
        old.NodeId = "legacy-node";
        old.NormalizeAccounts();
        Require(old.Accounts.Count==1 && old.Accounts[0].Cookie==old.Cookie && old.Accounts[0].NodeId=="legacy-node","Legacy account migration failed");
        var next=old.Copy();
        var second=new MuseAccount {Name="Second",Cookie="hatch_sess=account-two-secret"};next.Accounts.Add(second);
        next.SelectAccount(second.Id);
        Require(next.Cookie==second.Cookie && next.NodeId==second.NodeId && next.ApiKey==oldKey,"Account switch mixed credentials or changed the proxy key");
        Require(old.Accounts.Count==1 && old.Cookie!=next.Cookie,"Copy modified previous account");
        Require(ServerController.ConversationPath(root,old)!=ServerController.ConversationPath(root,next),"Account stores overlap");
        store.Save(next);var loaded=store.Load();
        Require(loaded.Accounts.Count==2&&loaded.ActiveAccountId==second.Id&&loaded.Cookie==second.Cookie&&loaded.ApiKey==oldKey,"Encrypted accounts or proxy key did not roundtrip");
        string cipher=System.Text.Encoding.UTF8.GetString(File.ReadAllBytes(Path.Combine(root,"settings.protected")));
        Require(!cipher.Contains("account-one-secret")&&!cipher.Contains("account-two-secret")&&!cipher.Contains(next.ApiKey),"Saved account secret was plaintext");
        checks.Add("Account migration, independent credentials/stores and stable proxy key across setup, switching and DPAPI reload");
        var proxy=new Proxy {OldId=old.ActiveAccountId};await proxy.StartAsync(old);
        Settings? saved=null;
        proxy.FailNew=true;
        bool rejected=false;try{await AccountSwitch.ApplyAsync(proxy,old,next,value=>saved=value);}catch(InvalidOperationException){rejected=true;}
        Require(rejected&&saved==null&&proxy.Running&&proxy.Current==old,"Failed switch did not restore previous account");
        proxy.FailNew=false;await AccountSwitch.ApplyAsync(proxy,old,next,value=>saved=value);
        Require(saved==next&&proxy.Current==next&&proxy.Running,"Successful switch not committed");
        proxy.Stop();await proxy.StartAsync(old);saved=null;
        proxy.FailNew=true;proxy.FailOld=true;rejected=false;
        try{await AccountSwitch.ApplyAsync(proxy,old,next,value=>saved=value);}catch(InvalidOperationException){rejected=true;}
        Require(rejected&&!proxy.Running&&saved==null,"Double connection failure left inconsistent state");
        var duplicate=next.Copy();duplicate.Accounts[1].Name=duplicate.Accounts[0].Name;
        rejected=false;try{SettingsStore.Validate(duplicate,false);}catch(ArgumentException){rejected=true;}
        Require(rejected,"Duplicate account labels accepted");
        var deleted=next.Copy();deleted.Accounts.RemoveAll(a=>a.Id==deleted.ActiveAccountId);deleted.ActiveAccountId=null;deleted.Cookie="";store.Save(deleted);
        var afterDeletion = store.Load();
        Require(afterDeletion.ActiveAccountId==null && afterDeletion.Cookie=="" && afterDeletion.ApiKey==oldKey,"Deleting active account reselected another account or changed the proxy key");
        checks.Add("Account switch success, rejection rollback, double-failure stop, duplicate names and active deletion");
    }
}
