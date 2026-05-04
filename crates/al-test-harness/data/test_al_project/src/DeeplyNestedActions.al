// T071 fixture: deeply-nested AL — page extension with nested action groups,
// part actions, and triggers. Exercises the `actions { area { group { group { ... } } } }`
// hierarchy that previously had no fixture coverage. Used by parser /
// definition / completion smoke tests under crates/al-test-harness/tests.
pageextension 50101 "Sales Order Pageext" extends "Sales Order"
{
    actions
    {
        addafter(Approve)
        {
            group("Outer Group")
            {
                Caption = 'Outer Group';

                group("Middle Group")
                {
                    Caption = 'Middle Group';

                    group("Inner Group")
                    {
                        Caption = 'Inner Group';

                        action("Deep Action 1")
                        {
                            Caption = 'Deep Action 1';
                            Image = Refresh;

                            trigger OnAction()
                            var
                                Result: Text;
                                Counter: Integer;
                            begin
                                for Counter := 1 to 5 do
                                    Result += Format(Counter);
                                Message('Deep action 1: %1', Result);
                            end;
                        }

                        action("Deep Action 2")
                        {
                            Caption = 'Deep Action 2';
                            Image = Calculate;

                            trigger OnAction()
                            var
                                Buffer: Text;
                                Idx: Integer;
                            begin
                                Idx := 0;
                                while Idx < 3 do begin
                                    Buffer := Buffer + StrSubstNo('item %1; ', Idx);
                                    Idx := Idx + 1;
                                end;
                                Message(Buffer);
                            end;
                        }
                    }

                    actionref("Deep Action 1 Ref"; "Deep Action 1") { }
                }
            }
        }

        addlast(Processing)
        {
            group("Processing Extras")
            {
                Caption = 'Processing Extras';

                action(LongRunningProcess)
                {
                    Caption = 'Long Running Process';
                    InFooterBar = true;

                    trigger OnAction()
                    var
                        Customer: Record Customer;
                        Total: Decimal;
                        Count: Integer;
                    begin
                        Customer.Reset();
                        if Customer.FindSet() then
                            repeat
                                Total += Customer."Balance (LCY)";
                                Count += 1;
                            until Customer.Next() = 0;
                        Message('Total %1 over %2 customers', Total, Count);
                    end;
                }
            }
        }
    }

    trigger OnOpenPage()
    var
        UserSetup: Record "User Setup";
    begin
        if UserSetup.Get(UserId) then
            CurrPage.Caption := UserSetup."Salespers./Purch. Code";
    end;
}
