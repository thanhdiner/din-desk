namespace MuseDesk;

internal static class GeminiModelTests
{
    public static async Task RunAsync(string root,List<string> checks)
    {
        var settings=new Settings{Cookie="hatch_sess=model-test"};settings.NormalizeAccounts();
        var account=new MuseAccount{Name="Model fixture",Provider="gemini",Cookie="__Secure-1PSID=model-test",GeminiAccountIndex=5,
            GeminiModelId="argon",GeminiThinkingId="4",GeminiModelName="4 Argon",GeminiThinkingName="Medium"};
        settings.Accounts.Add(account);settings.SelectAccount(account.Id);
        var store=new SettingsStore(root);store.Save(settings);var roundtrip=store.Load();
        if(roundtrip.GeminiAccountIndex!=5 || roundtrip.GeminiModelId!="argon" || roundtrip.GeminiThinkingId!="4")throw new InvalidOperationException("Model/account/thinking settings did not roundtrip");
        var zero=roundtrip.Copy();zero.Accounts[1].GeminiAccountIndex=0;zero.NormalizeAccounts();
        if(ServerController.ConversationPath(root,zero)==ServerController.ConversationPath(root,roundtrip))throw new InvalidOperationException("Google account indices share conversation storage");
        roundtrip.SelectAccount(roundtrip.Accounts[0].Id);
        if(roundtrip.Provider!="muse" || roundtrip.GeminiModelId!="" || roundtrip.GeminiAccountIndex!=0)throw new InvalidOperationException("Gemini model/account leaked into Muse settings");
        bool rejected=false;try{SettingsStore.ValidateGeminiSelection(100,"argon","4");}catch(ArgumentException){rejected=true;}
        if(!rejected)throw new InvalidOperationException("Invalid Google account index accepted");
        checks.Add("Google account index/model/thinking encrypted persistence, index-specific stores and unchanged Muse settings");

        var done=new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var ui=new Thread(()=>{
            try {
                int calls=0;
                var levels=new[]{new GeminiThinkingChoice("1","Low"),new GeminiThinkingChoice("4","Medium"),new GeminiThinkingChoice("2","High")};
                using var dialog=new GeminiModelDialog(account,root,(snapshot,_,_)=>{
                    calls++;
                    if(snapshot.GeminiAccountIndex!=5)throw new InvalidOperationException("Model picker ignored selected Google account");
                    return Task.FromResult(new GeminiModelCatalog(5,new[]{
                        new GeminiModelChoice("argon","4 Argon","Code",true,levels),new GeminiModelChoice("carbon","4 Carbon","Locked",false,Array.Empty<GeminiThinkingChoice>())}));
                });
                dialog.Shown+=(_,_)=>{
                    try {
                        var model=dialog.Controls.Find("GeminiModel",true).OfType<ComboBox>().Single();
                        var thinking=dialog.Controls.Find("GeminiThinking",true).OfType<ComboBox>().Single();
                        var save=dialog.Controls.Find("SaveGeminiModel",true).OfType<Button>().Single();
                        if(calls!=1 || dialog.ModelId!="argon" || dialog.ThinkingId!="4")throw new InvalidOperationException("Saved model/thinking was not restored in picker");
                        model.SelectedIndex=2;
                        if(save.Enabled)throw new InvalidOperationException("Locked model can be saved");
                        model.SelectedIndex=1;thinking.SelectedIndex=2;
                        if(dialog.AccountIndex!=5 || dialog.ThinkingId!="4" || thinking.Items.Count!=4 || !save.Enabled)throw new InvalidOperationException("Picker guessed medium enum or lost native levels");
                        var index=dialog.Controls.Find("GeminiAccountIndex",true).OfType<NumericUpDown>().Single();index.Value=6;
                        if(save.Enabled)throw new InvalidOperationException("Picker allowed saving catalog from another Google account");
                        index.Value=5;
                        if(calls!=1 || account.GeminiAccountIndex!=5 || account.GeminiModelId!="argon")throw new InvalidOperationException("Picker auto-refetched or mutated original account before Save");
                        done.TrySetResult();
                    }catch(Exception error){done.TrySetException(error);}
                    finally{dialog.Dispose();}
                };
                Application.Run(dialog);
            }catch(Exception error){done.TrySetException(error);}
        });
        ui.SetApartmentState(ApartmentState.STA);ui.IsBackground=true;ui.Start();await done.Task.WaitAsync(TimeSpan.FromSeconds(12));
        checks.Add("Native Gemini picker restores model/thinking, blocks locked model and stale-index save, and never changes account before Save");
    }
}
